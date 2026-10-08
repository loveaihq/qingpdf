//! The built-in encoding of an embedded font program (ISO 32000-1 9.6.6 and
//! 9.10.2): what character each code of a simple font stands for according to
//! the font file itself. It is the last resort of a simple font without a
//! `/BaseEncoding`, after `/ToUnicode` and the glyph names of `/Differences`.
//!
//! Three kinds of program are read, only as far as this takes:
//! - Type 1 (`/FontFile`): the cleartext part, `/Encoding StandardEncoding def`
//!   or the `dup <code> /<name> put` array (Type 1 Font Format, 2.2);
//! - CFF (`/FontFile3` `/Type1C`, or the `CFF ` table of an OpenType file): the
//!   Top DICT, the Encoding, the Charset and the String INDEX (Adobe Technical
//!   Note #5176), glyph names read through the Adobe Glyph List;
//! - TrueType (`/FontFile2`) of a symbolic font (9.6.6.4): the (3,0) or (1,0)
//!   `cmap` gives the glyph of a code, the glyph's Unicode value comes from the
//!   reversed (3,1) `cmap` or from the glyph names of the `post` table.
//!
//! Everything is read through checked accessors, every count is bounded by the
//! size of the data or by a fixed limit, and a program that does not fit is an
//! error: the caller carries on with the encoding it would have used anyway.

use std::collections::HashMap;

use super::cmap::Uni;
use super::data;

/// Which entry of the font descriptor the program came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `/FontFile`
    Type1,
    /// `/FontFile3`: bare CFF (`/Type1C`) or OpenType
    Cff,
    /// `/FontFile2`
    TrueType,
}

/// One entry per code: `None` where the font has no glyph for the code,
/// `Some(Uni::None)` where it has one that stands for no character we know.
pub(crate) type Table = Vec<Option<Uni>>;

pub(crate) type Res<T> = Result<T, &'static str>;

/// Most character codes a cmap enumeration visits (hostile segment tables overlap).
const MAX_CMAP_VISITS: usize = 1 << 18;
/// Most subtables of a `cmap` and most tables of an sfnt directory that are looked at.
const MAX_SUBTABLES: usize = 64;
const MAX_SFNT_TABLES: usize = 256;
/// Most tokens read from a Type 1 encoding array (256 entries take about 1000).
const MAX_T1_TOKENS: usize = 8000;

pub(crate) fn builtin_encoding(kind: Kind, program: &[u8]) -> Res<Table> {
    match kind {
        Kind::Type1 => type1(program),
        Kind::Cff => {
            if is_sfnt(program) {
                cff(sfnt_table(program, b"CFF ").ok_or("OpenType font without a CFF table")?)
            } else {
                cff(program)
            }
        }
        Kind::TrueType => truetype(program),
    }
}

/// A glyph name as the character it stands for (Adobe Glyph List).
pub(crate) fn uni_of_glyph_name(name: &str) -> Uni {
    let cps = data::glyph_name_to_unicode(name);
    match cps.as_slice() {
        [] => Uni::None,
        [one] => Uni::cp(*one),
        many => Uni::from_str(&many.iter().filter_map(|&c| char::from_u32(c)).collect::<String>()),
    }
}

fn standard_table() -> Table {
    data::STANDARD.iter().map(|&cp| (cp != 0).then(|| Uni::cp(u32::from(cp)))).collect()
}

fn named(name: &str) -> Option<Uni> {
    (name != ".notdef").then(|| uni_of_glyph_name(name))
}

/// The standard strings of CFF (Technical Note #5176, Appendix A) as a list: index `sid`.
pub(crate) fn cff_standard_string(sid: usize) -> Option<&'static str> {
    static LIST: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| CFF_STANDARD_STRINGS.split_ascii_whitespace().collect()).get(sid).copied()
}

/// The glyph name StandardEncoding gives a code (Adobe Type 1 Font Format, Appendix; Technical Note
/// #5176, Appendix B). The 149 characters of the encoding, in the order of their codes, are the
/// standard strings 1 to 149 of CFF.
pub(crate) fn standard_encoding_sid(code: u8) -> Option<u16> {
    if data::STANDARD.get(usize::from(code)).copied().unwrap_or(0) == 0 {
        return None;
    }
    let rank = data::STANDARD.iter().take(usize::from(code)).filter(|&&cp| cp != 0).count();
    u16::try_from(rank + 1).ok()
}

pub(crate) fn standard_encoding_name(code: u8) -> Option<&'static str> {
    cff_standard_string(usize::from(standard_encoding_sid(code)?))
}

// --- bytes ------------------------------------------------------------------------------------------

pub(crate) fn u8_at(d: &[u8], o: usize) -> Option<u8> {
    d.get(o).copied()
}

pub(crate) fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?))
}

pub(crate) fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

/// A little-endian number (the segment lengths of a PFB file).
pub(crate) fn u32_at_le(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

pub(crate) fn usize_at(d: &[u8], o: usize) -> Option<usize> {
    usize::try_from(u32_at(d, o)?).ok()
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub(crate) fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

// --- Type 1 -----------------------------------------------------------------------------------------

pub(crate) enum Tok<'a> {
    Name(&'a [u8]),
    Word(&'a [u8]),
}

pub(crate) struct Lexer<'a> {
    pub s: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn next_token(&mut self) -> Option<Tok<'a>> {
        loop {
            let b = u8_at(self.s, self.pos)?;
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while u8_at(self.s, self.pos).is_some_and(|c| c != b'\n' && c != b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        let start = self.pos;
        let b = u8_at(self.s, start)?;
        let delimiter = |c: u8| is_ws(c) || matches!(c, b'/' | b'{' | b'}' | b'[' | b']' | b'%');
        if b == b'/' {
            self.pos += 1;
            let from = self.pos;
            while u8_at(self.s, self.pos).is_some_and(|c| !delimiter(c)) {
                self.pos += 1;
            }
            return Some(Tok::Name(self.s.get(from..self.pos)?));
        }
        self.pos += 1;
        if !matches!(b, b'{' | b'}' | b'[' | b']') {
            while u8_at(self.s, self.pos).is_some_and(|c| !delimiter(c)) {
                self.pos += 1;
            }
        }
        Some(Tok::Word(self.s.get(start..self.pos)?))
    }
}

/// A code of a `dup` entry: decimal, or `radix#digits`.
fn t1_code(word: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(word).ok()?;
    let v = match text.split_once('#') {
        Some((radix, digits)) => u32::from_str_radix(digits, radix.parse().ok().filter(|r| (2..=36).contains(r))?).ok()?,
        None => text.parse::<u32>().ok()?,
    };
    usize::try_from(v).ok().filter(|&c| c < 256)
}

fn type1(program: &[u8]) -> Res<Table> {
    Ok(match type1_encoding(program)? {
        None => standard_table(),
        Some(names) => names.iter().map(|n| n.as_ref().and_then(|n| named(&String::from_utf8_lossy(n)))).collect(),
    })
}

/// The glyph name the cleartext part of a Type 1 program gives each code; `None` for
/// `/Encoding StandardEncoding def`.
pub(crate) fn type1_encoding(program: &[u8]) -> Res<Option<Vec<Option<Vec<u8>>>>> {
    // Only the cleartext part, which ends where the encrypted part begins.
    let clear = program.get(..find(program, b"eexec").unwrap_or(program.len())).unwrap_or(program);
    let at = find(clear, b"/Encoding").ok_or("no /Encoding in the cleartext part")?;
    let mut lexer = Lexer { s: clear.get(at + 9..).unwrap_or(&[]), pos: 0 };
    let mut tok = lexer.next_token();
    match &tok {
        Some(Tok::Word(w)) if *w == b"StandardEncoding" => return Ok(None),
        Some(Tok::Word(w)) if w.first().is_some_and(u8::is_ascii_alphabetic) && *w != b"dup" => {
            return Err("a predefined encoding other than StandardEncoding");
        }
        _ => {}
    }
    let mut table: Vec<Option<Vec<u8>>> = vec![None; 256];
    // 0: nothing, 1: after `dup`, 2: after `dup <code>`
    let mut state = 0u8;
    let mut code = 0usize;
    let mut entries = 0usize;
    for _ in 0..MAX_T1_TOKENS {
        let Some(t) = tok else { break };
        match t {
            Tok::Word(w) if w == b"def" => break,
            Tok::Word(w) if w == b"dup" => state = 1,
            Tok::Word(w) if state == 1 => match t1_code(w) {
                Some(c) => {
                    code = c;
                    state = 2;
                }
                None => state = 0,
            },
            Tok::Name(n) if state == 2 => {
                if let Some(slot) = table.get_mut(code) {
                    *slot = (n != b".notdef").then(|| n.to_vec());
                    entries += 1;
                }
                state = 0;
            }
            _ => state = 0,
        }
        tok = lexer.next_token();
    }
    if entries == 0 {
        return Err("no encoding array in the cleartext part");
    }
    Ok(Some(table))
}

// --- CFF --------------------------------------------------------------------------------------------

/// An INDEX (Technical Note #5176, 5): where its objects are.
pub(crate) struct Index {
    pub count: usize,
    off_size: usize,
    offsets: usize,
    /// Object offsets are relative to the byte before the first object.
    base: usize,
    pub end: usize,
}

impl Index {
    pub fn parse(d: &[u8], pos: usize) -> Res<Index> {
        const BAD: &str = "bad INDEX in the CFF data";
        let count = usize::from(u16_at(d, pos).ok_or(BAD)?);
        if count == 0 {
            return Ok(Index { count: 0, off_size: 1, offsets: 0, base: 0, end: pos + 2 });
        }
        let off_size = usize::from(u8_at(d, pos + 2).ok_or(BAD)?);
        if !(1..=4).contains(&off_size) {
            return Err(BAD);
        }
        let offsets = pos + 3;
        let base = offsets + (count + 1) * off_size - 1;
        let mut index = Index { count, off_size, offsets, base, end: 0 };
        let last = index.offset(d, count).ok_or(BAD)?;
        index.end = base.checked_add(last).filter(|&e| last >= 1 && e <= d.len()).ok_or(BAD)?;
        Ok(index)
    }

    fn offset(&self, d: &[u8], i: usize) -> Option<usize> {
        let at = self.offsets.checked_add(i.checked_mul(self.off_size)?)?;
        d.get(at..at.checked_add(self.off_size)?)?.iter().try_fold(0usize, |acc, &b| acc.checked_mul(256)?.checked_add(usize::from(b)))
    }

    pub fn get<'a>(&self, d: &'a [u8], i: usize) -> Option<&'a [u8]> {
        if i >= self.count {
            return None;
        }
        let (a, b) = (self.offset(d, i)?, self.offset(d, i + 1)?);
        if a < 1 || b < a {
            return None;
        }
        d.get(self.base.checked_add(a)?..self.base.checked_add(b)?)
    }
}

/// The (operator, operands) pairs of a DICT. Operators of two bytes are 1200 plus the second. At
/// most 256 entries of at most 48 operands each are kept (Technical Note #5176, 4).
pub(crate) fn dict_operands(d: &[u8]) -> Res<Vec<(u16, Vec<f64>)>> {
    const BAD: &str = "bad DICT in the CFF data";
    const MAX_OPERANDS: usize = 48;
    let mut out = Vec::new();
    let mut args: Vec<f64> = Vec::new();
    let mut pos = 0usize;
    let push = |args: &mut Vec<f64>, v: f64| {
        if args.len() < MAX_OPERANDS {
            args.push(v);
        }
    };
    while let Some(b0) = u8_at(d, pos) {
        pos += 1;
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    pos += 1;
                    1200 + u16::from(u8_at(d, pos - 1).ok_or(BAD)?)
                } else {
                    u16::from(b0)
                };
                if out.len() < 256 {
                    out.push((op, std::mem::take(&mut args)));
                } else {
                    args.clear();
                }
            }
            28 => {
                push(&mut args, f64::from(i16::from_be_bytes(u16_at(d, pos).ok_or(BAD)?.to_be_bytes())));
                pos += 2;
            }
            29 => {
                push(&mut args, f64::from(i32::from_be_bytes(u32_at(d, pos).ok_or(BAD)?.to_be_bytes())));
                pos += 4;
            }
            30 => {
                // A real number: nibbles up to the 0xf end marker.
                let mut text = String::new();
                'nibbles: loop {
                    let b = u8_at(d, pos).ok_or(BAD)?;
                    pos += 1;
                    for nib in [b >> 4, b & 0x0f] {
                        match nib {
                            0..=9 => text.push(char::from(b'0' + nib)),
                            0xa => text.push('.'),
                            0xb => text.push('E'),
                            0xc => text.push_str("E-"),
                            0xe => text.push('-'),
                            0xf => break 'nibbles,
                            _ => {}
                        }
                    }
                    if text.len() > 64 {
                        return Err(BAD);
                    }
                }
                push(&mut args, text.parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0));
            }
            32..=246 => push(&mut args, f64::from(b0) - 139.0),
            247..=250 => {
                push(&mut args, (f64::from(b0) - 247.0) * 256.0 + f64::from(u8_at(d, pos).ok_or(BAD)?) + 108.0);
                pos += 1;
            }
            251..=254 => {
                push(&mut args, -(f64::from(b0) - 251.0) * 256.0 - f64::from(u8_at(d, pos).ok_or(BAD)?) - 108.0);
                pos += 1;
            }
            _ => return Err(BAD),
        }
    }
    Ok(out)
}

/// The (operator, last operand) pairs of a DICT.
fn dict_entries(d: &[u8]) -> Res<Vec<(u16, f64)>> {
    Ok(dict_operands(d)?.into_iter().map(|(op, args)| (op, args.last().copied().unwrap_or(0.0))).collect())
}

pub(crate) fn offset_operand(v: f64) -> Option<usize> {
    (0.0..1e9).contains(&v).then_some(v as usize)
}

/// The name of a string id: the 391 standard strings, then the String INDEX.
fn sid_name(sid: u16, strings: &Index, d: &[u8]) -> Option<String> {
    let sid = usize::from(sid);
    let bytes = match sid.checked_sub(391) {
        None => return CFF_STANDARD_STRINGS.split_ascii_whitespace().nth(sid).map(str::to_string),
        Some(i) => strings.get(d, i)?,
    };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Glyph id to string id for `n` glyphs (Technical Note #5176, 13).
pub(crate) fn charset(d: &[u8], off: usize, n: usize) -> Res<Vec<u16>> {
    const BAD: &str = "bad charset in the CFF data";
    let mut sids = vec![0u16; n];
    match off {
        // ISOAdobe: glyph i is string i, for the 229 standard strings it covers.
        0 => {
            for (i, slot) in sids.iter_mut().enumerate().take(229) {
                *slot = u16::try_from(i).unwrap_or(0);
            }
        }
        1 | 2 => return Err("a predefined Expert charset"),
        _ => {
            let format = u8_at(d, off).ok_or(BAD)?;
            let mut pos = off + 1;
            let mut gid = 1usize;
            match format {
                0 => {
                    while gid < n {
                        let sid = u16_at(d, pos).ok_or(BAD)?;
                        pos += 2;
                        if let Some(slot) = sids.get_mut(gid) {
                            *slot = sid;
                        }
                        gid += 1;
                    }
                }
                1 | 2 => {
                    while gid < n {
                        let first = u16_at(d, pos).ok_or(BAD)?;
                        let left = if format == 1 {
                            pos += 3;
                            usize::from(u8_at(d, pos - 1).ok_or(BAD)?)
                        } else {
                            pos += 4;
                            usize::from(u16_at(d, pos - 2).ok_or(BAD)?)
                        };
                        for i in 0..=left {
                            if gid >= n {
                                break;
                            }
                            if let Some(slot) = sids.get_mut(gid) {
                                *slot = first.wrapping_add(u16::try_from(i).unwrap_or(0));
                            }
                            gid += 1;
                        }
                    }
                }
                _ => return Err(BAD),
            }
        }
    }
    Ok(sids)
}

/// A custom CFF Encoding (formats 0 and 1, and the supplement; Technical Note #5176, 12): the glyph
/// of each code (0: none) and the (code, string id) pairs of the supplement.
pub(crate) type CffEncoding = ([usize; 256], Vec<(usize, u16)>);

pub(crate) fn cff_encoding(d: &[u8], encoding: usize) -> Res<CffEncoding> {
    const BAD_ENC: &str = "bad Encoding in the CFF data";
    let format = u8_at(d, encoding).ok_or(BAD_ENC)?;
    let mut pos = encoding + 1;
    let mut gid_of_code = [0usize; 256];
    match format & 0x7f {
        0 => {
            let n = usize::from(u8_at(d, pos).ok_or(BAD_ENC)?);
            for i in 0..n {
                let code = usize::from(u8_at(d, pos + 1 + i).ok_or(BAD_ENC)?);
                if let Some(slot) = gid_of_code.get_mut(code) {
                    *slot = i + 1;
                }
            }
            pos += 1 + n;
        }
        1 => {
            let n = usize::from(u8_at(d, pos).ok_or(BAD_ENC)?);
            pos += 1;
            let mut gid = 1usize;
            for _ in 0..n {
                let first = usize::from(u8_at(d, pos).ok_or(BAD_ENC)?);
                let left = usize::from(u8_at(d, pos + 1).ok_or(BAD_ENC)?);
                pos += 2;
                for code in first..=first + left {
                    if let Some(slot) = gid_of_code.get_mut(code) {
                        *slot = gid;
                    }
                    gid += 1;
                }
            }
        }
        _ => return Err(BAD_ENC),
    }
    let mut supplement = Vec::new();
    if format & 0x80 != 0 {
        let n = usize::from(u8_at(d, pos).ok_or(BAD_ENC)?);
        for i in 0..n {
            let at = pos + 1 + 3 * i;
            let code = usize::from(u8_at(d, at).ok_or(BAD_ENC)?);
            let sid = u16_at(d, at + 1).ok_or(BAD_ENC)?;
            supplement.push((code, sid));
        }
    }
    Ok((gid_of_code, supplement))
}

fn cff(d: &[u8]) -> Res<Table> {
    const BAD: &str = "bad CFF header";
    if u8_at(d, 0) != Some(1) {
        return Err("not a CFF font (version 1)");
    }
    let header = usize::from(u8_at(d, 2).ok_or(BAD)?);
    let names = Index::parse(d, header)?;
    let tops = Index::parse(d, names.end)?;
    let strings = Index::parse(d, tops.end)?;
    let top = dict_entries(tops.get(d, 0).ok_or("no Top DICT")?)?;
    let operand = |op: u16| top.iter().find(|(o, _)| *o == op).map(|&(_, v)| v);
    if operand(1230).is_some() {
        return Err("a CID-keyed CFF font");
    }
    let encoding = operand(16).map_or(Some(0), offset_operand).ok_or("bad Encoding offset")?;
    if encoding == 0 {
        return Ok(standard_table());
    }
    if encoding == 1 {
        return Err("the predefined Expert encoding");
    }
    let charstrings = operand(17).and_then(offset_operand).ok_or("no CharStrings in the Top DICT")?;
    let glyphs = Index::parse(d, charstrings)?.count;
    let sids = charset(d, operand(15).map_or(Some(0), offset_operand).ok_or("bad charset offset")?, glyphs)?;

    let (gid_of_code, supplement) = cff_encoding(d, encoding)?;
    let mut table: Table = vec![None; 256];
    for (code, slot) in table.iter_mut().enumerate() {
        let gid = gid_of_code.get(code).copied().unwrap_or(0);
        if gid > 0 {
            let name = sids.get(gid).and_then(|&sid| sid_name(sid, &strings, d));
            *slot = name.as_deref().and_then(named);
        }
    }
    for (code, sid) in supplement {
        if let (Some(slot), Some(name)) = (table.get_mut(code), sid_name(sid, &strings, d)) {
            *slot = named(&name);
        }
    }
    Ok(table)
}

// --- sfnt and TrueType ------------------------------------------------------------------------------

pub(crate) fn is_sfnt(d: &[u8]) -> bool {
    matches!(d.get(..4), Some([0, 1, 0, 0] | b"true" | b"OTTO" | b"ttcf"))
}

pub(crate) fn sfnt_table<'a>(d: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    sfnt_table_face(d, 0, tag)
}

/// The number of fonts in a TrueType collection (1 for a plain font).
pub(crate) fn sfnt_faces(d: &[u8]) -> usize {
    if d.get(..4) == Some(b"ttcf") { usize_at(d, 8).unwrap_or(0).min(64) } else { 1 }
}

/// Where the table directory of font number `face` starts (a collection has several).
pub(crate) fn sfnt_directory(d: &[u8], face: usize) -> Option<usize> {
    if d.get(..4) == Some(b"ttcf") {
        if face >= sfnt_faces(d) {
            return None;
        }
        usize_at(d, 12usize.checked_add(face.checked_mul(4)?)?)
    } else {
        (face == 0).then_some(0)
    }
}

/// A table of font number `face`: (offset, length) in the file, both inside it.
pub(crate) fn sfnt_table_range(d: &[u8], face: usize, tag: &[u8; 4]) -> Option<(usize, usize)> {
    let base = sfnt_directory(d, face)?;
    let n = usize::from(u16_at(d, base.checked_add(4)?)?).min(MAX_SFNT_TABLES);
    for i in 0..n {
        let rec = base.checked_add(12)?.checked_add(i * 16)?;
        if d.get(rec..rec.checked_add(4)?) == Some(tag) {
            let off = usize_at(d, rec.checked_add(8)?)?;
            let len = usize_at(d, rec.checked_add(12)?)?;
            return (off.checked_add(len)? <= d.len()).then_some((off, len));
        }
    }
    None
}

pub(crate) fn sfnt_table_face<'a>(d: &'a [u8], face: usize, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let (off, len) = sfnt_table_range(d, face, tag)?;
    d.get(off..off + len)
}

pub(crate) struct Sub {
    pub platform: u16,
    pub encoding: u16,
    pub off: usize,
}

pub(crate) fn cmap_subtables(cmap: &[u8]) -> Vec<Sub> {
    let n = usize::from(u16_at(cmap, 2).unwrap_or(0)).min(MAX_SUBTABLES);
    (0..n)
        .filter_map(|i| {
            let rec = 4 + i * 8;
            Some(Sub { platform: u16_at(cmap, rec)?, encoding: u16_at(cmap, rec + 2)?, off: usize_at(cmap, rec + 4)? })
        })
        .collect()
}

/// The glyph of segment `i` of a format 4 subtable for `code` (a code inside the segment).
fn format4_glyph(c: &[u8], off: usize, seg: usize, i: usize, code: u32) -> Option<u16> {
    let start = u32::from(u16_at(c, off + 16 + 2 * seg + 2 * i)?);
    let delta = u16_at(c, off + 16 + 4 * seg + 2 * i)?;
    let ro_pos = off + 16 + 6 * seg + 2 * i;
    let ro = usize::from(u16_at(c, ro_pos)?);
    let glyph = if ro == 0 {
        u16::try_from(code & 0xffff).ok()?
    } else {
        let at = ro_pos.checked_add(ro)?.checked_add(2 * usize::try_from(code.checked_sub(start)?).ok()?)?;
        u16_at(c, at).filter(|&g| g != 0)?
    };
    Some(glyph.wrapping_add(delta))
}

/// The glyph a cmap subtable gives a code (formats 0, 4, 6 and 12); `None` for no glyph.
pub(crate) fn cmap_lookup(c: &[u8], off: usize, code: u32) -> Option<u16> {
    let gid = match u16_at(c, off)? {
        0 => u8_at(c, off + 6 + usize::try_from(code).ok().filter(|&v| v < 256)?).map(u16::from)?,
        4 => {
            let seg = usize::from(u16_at(c, off + 6)? / 2);
            // The end codes are sorted (the spec says so): the first segment that ends at or after code.
            let (mut lo, mut hi) = (0usize, seg);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if u32::from(u16_at(c, off + 14 + 2 * mid)?) < code {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if lo >= seg || u32::from(u16_at(c, off + 16 + 2 * seg + 2 * lo)?) > code {
                return None;
            }
            format4_glyph(c, off, seg, lo, code)?
        }
        6 => {
            let first = u32::from(u16_at(c, off + 6)?);
            let n = usize::from(u16_at(c, off + 8)?);
            let i = usize::try_from(code.checked_sub(first)?).ok().filter(|&i| i < n)?;
            u16_at(c, off + 10 + 2 * i)?
        }
        12 => {
            let n = usize_at(c, off + 12)?.min(c.len() / 12);
            let group = |i: usize| -> Option<(u32, u32, u32)> {
                let at = off.checked_add(16)?.checked_add(i * 12)?;
                Some((u32_at(c, at)?, u32_at(c, at + 4)?, u32_at(c, at + 8)?))
            };
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if group(mid)?.1 < code {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            let (start, _, glyph) = group(lo).filter(|g| g.0 <= code && code <= g.1)?;
            u16::try_from(glyph.checked_add(code - start)?).ok()?
        }
        _ => return None,
    };
    (gid != 0).then_some(gid)
}

/// Every (code, glyph) pair of a cmap subtable, visiting at most `left` codes.
fn cmap_enumerate(c: &[u8], off: usize, left: &mut usize, f: &mut dyn FnMut(u32, u16)) {
    let Some(format) = u16_at(c, off) else { return };
    let mut visit = |code: u32, gid: Option<u16>, left: &mut usize| -> bool {
        if *left == 0 {
            return false;
        }
        *left -= 1;
        if let Some(g) = gid.filter(|&g| g != 0) {
            f(code, g);
        }
        true
    };
    match format {
        0 => {
            for code in 0..256u32 {
                if !visit(code, cmap_lookup(c, off, code), left) {
                    return;
                }
            }
        }
        4 => {
            let Some(seg) = u16_at(c, off + 6).map(|v| usize::from(v / 2)) else { return };
            for i in 0..seg {
                let (Some(end), Some(start)) = (u16_at(c, off + 14 + 2 * i), u16_at(c, off + 16 + 2 * seg + 2 * i)) else { return };
                for code in u32::from(start)..=u32::from(end) {
                    if code != 0xffff && !visit(code, format4_glyph(c, off, seg, i, code), left) {
                        return;
                    }
                }
            }
        }
        6 => {
            let (Some(first), Some(n)) = (u16_at(c, off + 6), u16_at(c, off + 8)) else { return };
            for i in 0..usize::from(n) {
                let gid = u16_at(c, off + 10 + 2 * i);
                if !visit(u32::from(first) + u32::try_from(i).unwrap_or(0), gid, left) {
                    return;
                }
            }
        }
        12 => {
            let Some(n) = usize_at(c, off + 12) else { return };
            for i in 0..n.min(c.len() / 12) {
                let at = off + 16 + i * 12;
                let (Some(start), Some(end), Some(glyph)) = (u32_at(c, at), u32_at(c, at + 4), u32_at(c, at + 8)) else { return };
                for code in start..=end.max(start) {
                    let gid = glyph.checked_add(code - start).and_then(|g| u16::try_from(g).ok());
                    if !visit(code, gid, left) {
                        return;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Glyph names of a `post` table of format 2.0.
pub(crate) struct Post<'a> {
    table: &'a [u8],
    glyphs: usize,
    names: Vec<&'a [u8]>,
}

impl<'a> Post<'a> {
    pub fn parse(table: &'a [u8]) -> Option<Post<'a>> {
        if u32_at(table, 0)? != 0x0002_0000 {
            return None;
        }
        let glyphs = usize::from(u16_at(table, 32)?);
        let mut pos = 34 + 2 * glyphs;
        let mut names = Vec::new();
        while let Some(len) = u8_at(table, pos) {
            names.push(table.get(pos + 1..pos + 1 + usize::from(len))?);
            pos += 1 + usize::from(len);
        }
        Some(Post { table, glyphs, names })
    }

    pub fn name(&self, gid: u16) -> Option<String> {
        if usize::from(gid) >= self.glyphs {
            return None;
        }
        let i = usize::from(u16_at(self.table, 34 + 2 * usize::from(gid))?);
        match i.checked_sub(258) {
            None => MAC_GLYPH_NAMES.split_ascii_whitespace().nth(i).map(str::to_string),
            Some(j) => self.names.get(j).map(|n| String::from_utf8_lossy(n).into_owned()),
        }
    }
}

/// 9.6.6.4 for a font read through its own cmap.
fn truetype(d: &[u8]) -> Res<Table> {
    let cmap = sfnt_table(d, b"cmap").ok_or("no cmap table")?;
    let subs = cmap_subtables(cmap);
    let find_sub = |platform: u16, encoding: u16| subs.iter().find(|s| s.platform == platform && s.encoding == encoding);
    let (symbol, mac) = (find_sub(3, 0), find_sub(1, 0));
    if symbol.is_none() && mac.is_none() {
        return Err("no (3,0) or (1,0) cmap subtable");
    }
    // 9.6.6.4: the (3,0) subtable takes the code itself or the code in one of the ranges at 0xF000,
    // 0xF100 and 0xF200; the (1,0) subtable takes the code.
    let glyph_of = |code: u32| -> Option<u16> {
        symbol
            .and_then(|s| [0, 0xF000, 0xF100, 0xF200].iter().find_map(|base| cmap_lookup(cmap, s.off, base + code)))
            .or_else(|| mac.and_then(|s| cmap_lookup(cmap, s.off, code)))
    };
    let glyphs: Vec<Option<u16>> = (0..256u32).map(glyph_of).collect();
    let mut table: Table = vec![None; 256];
    if glyphs.iter().all(Option::is_none) {
        return Ok(table);
    }
    // The Unicode value of a glyph: the reversed Unicode cmap (lowest code wins), else its name.
    let mut reverse: HashMap<u16, u32> = HashMap::new();
    let unicode_sub = find_sub(3, 10).or_else(|| find_sub(3, 1)).or_else(|| subs.iter().find(|s| s.platform == 0));
    if let Some(s) = unicode_sub {
        let mut left = MAX_CMAP_VISITS;
        cmap_enumerate(cmap, s.off, &mut left, &mut |code, gid| {
            let entry = reverse.entry(gid).or_insert(code);
            *entry = (*entry).min(code);
        });
    }
    let post = sfnt_table(d, b"post").and_then(Post::parse);
    for (slot, gid) in table.iter_mut().zip(glyphs) {
        let Some(gid) = gid else { continue };
        *slot = Some(match (reverse.get(&gid), post.as_ref().and_then(|p| p.name(gid))) {
            (Some(&cp), _) => Uni::cp(cp),
            (None, Some(name)) => uni_of_glyph_name(&name),
            (None, None) => Uni::None,
        });
    }
    Ok(table)
}

/// The 391 standard strings of CFF (Adobe Technical Note #5176, Appendix A): SID n is the n-th word.
const CFF_STANDARD_STRINGS: &str = "\n    .notdef space exclam quotedbl numbersign dollar percent ampersand quoteright parenleft parenright \n    asterisk plus comma hyphen period slash zero one two three four five six seven eight nine colon \n    semicolon less equal greater question at A B C D E F G H I J K L M N O P Q R S T U V W X Y Z bracketleft \n    backslash bracketright asciicircum underscore quoteleft a b c d e f g h i j k l m n o p q r s t u v w x \n    y z braceleft bar braceright asciitilde exclamdown cent sterling fraction yen florin section currency \n    quotesingle quotedblleft guillemotleft guilsinglleft guilsinglright fi fl endash dagger daggerdbl \n    periodcentered paragraph bullet quotesinglbase quotedblbase quotedblright guillemotright ellipsis \n    perthousand questiondown grave acute circumflex tilde macron breve dotaccent dieresis ring cedilla \n    hungarumlaut ogonek caron emdash AE ordfeminine Lslash Oslash OE ordmasculine ae dotlessi lslash oslash \n    oe germandbls onesuperior logicalnot mu trademark Eth onehalf plusminus Thorn onequarter divide \n    brokenbar degree thorn threequarters twosuperior registered minus eth multiply threesuperior copyright \n    Aacute Acircumflex Adieresis Agrave Aring Atilde Ccedilla Eacute Ecircumflex Edieresis Egrave Iacute \n    Icircumflex Idieresis Igrave Ntilde Oacute Ocircumflex Odieresis Ograve Otilde Scaron Uacute Ucircumflex \n    Udieresis Ugrave Yacute Ydieresis Zcaron aacute acircumflex adieresis agrave aring atilde ccedilla \n    eacute ecircumflex edieresis egrave iacute icircumflex idieresis igrave ntilde oacute ocircumflex \n    odieresis ograve otilde scaron uacute ucircumflex udieresis ugrave yacute ydieresis zcaron exclamsmall \n    Hungarumlautsmall dollaroldstyle dollarsuperior ampersandsmall Acutesmall parenleftsuperior \n    parenrightsuperior twodotenleader onedotenleader zerooldstyle oneoldstyle twooldstyle threeoldstyle \n    fouroldstyle fiveoldstyle sixoldstyle sevenoldstyle eightoldstyle nineoldstyle commasuperior \n    threequartersemdash periodsuperior questionsmall asuperior bsuperior centsuperior dsuperior esuperior \n    isuperior lsuperior msuperior nsuperior osuperior rsuperior ssuperior tsuperior ff ffi ffl \n    parenleftinferior parenrightinferior Circumflexsmall hyphensuperior Gravesmall Asmall Bsmall Csmall \n    Dsmall Esmall Fsmall Gsmall Hsmall Ismall Jsmall Ksmall Lsmall Msmall Nsmall Osmall Psmall Qsmall Rsmall \n    Ssmall Tsmall Usmall Vsmall Wsmall Xsmall Ysmall Zsmall colonmonetary onefitted rupiah Tildesmall \n    exclamdownsmall centoldstyle Lslashsmall Scaronsmall Zcaronsmall Dieresissmall Brevesmall Caronsmall \n    Dotaccentsmall Macronsmall figuredash hypheninferior Ogoneksmall Ringsmall Cedillasmall \n    questiondownsmall oneeighth threeeighths fiveeighths seveneighths onethird twothirds zerosuperior \n    foursuperior fivesuperior sixsuperior sevensuperior eightsuperior ninesuperior zeroinferior oneinferior \n    twoinferior threeinferior fourinferior fiveinferior sixinferior seveninferior eightinferior nineinferior \n    centinferior dollarinferior periodinferior commainferior Agravesmall Aacutesmall Acircumflexsmall \n    Atildesmall Adieresissmall Aringsmall AEsmall Ccedillasmall Egravesmall Eacutesmall Ecircumflexsmall \n    Edieresissmall Igravesmall Iacutesmall Icircumflexsmall Idieresissmall Ethsmall Ntildesmall Ogravesmall \n    Oacutesmall Ocircumflexsmall Otildesmall Odieresissmall OEsmall Oslashsmall Ugravesmall Uacutesmall \n    Ucircumflexsmall Udieresissmall Yacutesmall Thornsmall Ydieresissmall 001.000 001.001 001.002 001.003 \n    Black Bold Book Light Medium Regular Roman Semibold";

/// The 258 glyph names of the standard Macintosh glyph ordering (OpenType `post` table, format 2).
const MAC_GLYPH_NAMES: &str = "\n    .notdef .null nonmarkingreturn space exclam quotedbl numbersign dollar percent ampersand quotesingle \n    parenleft parenright asterisk plus comma hyphen period slash zero one two three four five six seven \n    eight nine colon semicolon less equal greater question at A B C D E F G H I J K L M N O P Q R S T U V W \n    X Y Z bracketleft backslash bracketright asciicircum underscore grave a b c d e f g h i j k l m n o p q \n    r s t u v w x y z braceleft bar braceright asciitilde Adieresis Aring Ccedilla Eacute Ntilde Odieresis \n    Udieresis aacute agrave acircumflex adieresis atilde aring ccedilla eacute egrave ecircumflex edieresis \n    iacute igrave icircumflex idieresis ntilde oacute ograve ocircumflex odieresis otilde uacute ugrave \n    ucircumflex udieresis dagger degree cent sterling section bullet paragraph germandbls registered \n    copyright trademark acute dieresis notequal AE Oslash infinity plusminus lessequal greaterequal yen mu \n    partialdiff summation product pi integral ordfeminine ordmasculine Omega ae oslash questiondown \n    exclamdown logicalnot radical florin approxequal Delta guillemotleft guillemotright ellipsis \n    nonbreakingspace Agrave Atilde Otilde OE oe endash emdash quotedblleft quotedblright quoteleft \n    quoteright divide lozenge ydieresis Ydieresis fraction currency guilsinglleft guilsinglright fi fl \n    daggerdbl periodcentered quotesinglbase quotedblbase perthousand Acircumflex Ecircumflex Aacute \n    Edieresis Egrave Iacute Icircumflex Idieresis Igrave Oacute Ocircumflex apple Ograve Uacute Ucircumflex \n    Ugrave dotlessi circumflex tilde macron breve dotaccent ring cedilla hungarumlaut ogonek caron Lslash \n    lslash Scaron scaron Zcaron zcaron brokenbar Eth eth Yacute yacute Thorn thorn minus multiply \n    onesuperior twosuperior threesuperior onehalf onequarter threequarters franc Gbreve gbreve Idotaccent \n    Scedilla scedilla Cacute cacute Ccaron ccaron dcroat";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::testutil::PdfBuilder;
    use crate::text::TextExtractor;

    /// The text of a table entry: `-` for no glyph, `?` for a glyph without a character.
    fn show(t: &Table, code: usize) -> String {
        match t.get(code) {
            Some(Some(Uni::One(c))) => c.to_string(),
            Some(Some(Uni::Many(s))) => s.to_string(),
            Some(Some(Uni::None)) => "?".to_string(),
            _ => "-".to_string(),
        }
    }

    fn shown(t: &Table, codes: &[usize]) -> String {
        codes.iter().map(|&c| show(t, c)).collect::<Vec<_>>().join(" ")
    }

    // --- the names -------------------------------------------------------------------------------

    #[test]
    fn the_name_lists_have_their_counts_and_known_entries() {
        let std: Vec<&str> = CFF_STANDARD_STRINGS.split_ascii_whitespace().collect();
        assert_eq!(std.len(), 391);
        assert_eq!((std[0], std[1], std[34], std[66], std[228], std[274], std[390]), (".notdef", "space", "A", "a", "zcaron", "Asmall", "Semibold"));
        // SIDs 1 to 149 are the Standard Encoding in code order: the names must give the Unicode
        // values of the table the spec gave us.
        let codes: Vec<usize> = (0..256).filter(|&c| data::STANDARD.get(c).is_some_and(|&u| u != 0)).collect();
        assert_eq!(codes.len(), 149);
        for (i, &code) in codes.iter().enumerate() {
            let want = Uni::cp(u32::from(data::STANDARD[code]));
            assert_eq!(uni_of_glyph_name(std[i + 1]), want, "SID {} for code {code}", i + 1);
        }
        let mac: Vec<&str> = MAC_GLYPH_NAMES.split_ascii_whitespace().collect();
        assert_eq!(mac.len(), 258);
        assert_eq!((mac[0], mac[3], mac[36], mac[68], mac[98], mac[210], mac[257]), (".notdef", "space", "A", "a", "Adieresis", "apple", "dcroat"));
        // Macintosh glyphs 3 to 97 are ASCII 32 to 126, with the glyph names of the Mac Roman set above.
        for (i, name) in mac.iter().enumerate().take(98).skip(3) {
            let ascii = char::from_u32(u32::try_from(i + 29).unwrap_or(0)).unwrap_or('?');
            assert_eq!(uni_of_glyph_name(name), Uni::cp(u32::from(ascii)), "Macintosh glyph {i}");
        }
        // Glyphs 98 to 225 are the Mac Roman codes 0x80 to 0xFF in order (AGL's Omega is the Ohm sign).
        for (i, name) in mac.iter().enumerate().take(226).skip(98) {
            let cp = data::MAC_ROMAN[0x80 + i - 98];
            if cp != 0 && *name != "Omega" {
                assert_eq!(uni_of_glyph_name(name), Uni::cp(u32::from(cp)), "Macintosh glyph {i} {name}");
            }
        }
    }

    // --- Type 1 ------------------------------------------------------------------------------------

    const TYPE1: &[u8] = b"%!PS-AdobeFont-1.0: Foo 1.0\n/FontName /Foo def\n/Encoding 256 array\n\
        0 1 255 {1 index exch /.notdef put} for\ndup 65 /X put\ndup 66/Y put %comment /Q put\n\
        dup 8#103 /uni4E2D put\ndup 200 /g99 put\ndup 300 /W put\nreadonly def\n\
        /FontBBox{0 0 1 1}readonly def\ncurrentfile eexec\n\x80\x81 dup 70 /Z put def";

    #[test]
    fn type1_encoding_array() {
        let t = builtin_encoding(Kind::Type1, TYPE1).unwrap();
        assert_eq!(shown(&t, &[65, 66, 67, 200, 70, 0, 300]), "X Y 中 ? - - -");
    }

    #[test]
    fn type1_standard_encoding_and_other_forms() {
        let t = builtin_encoding(Kind::Type1, b"/FontName /F def /Encoding StandardEncoding def currentfile eexec").unwrap();
        assert_eq!(shown(&t, &[65, 174, 1]), "A fi -");
        assert!(builtin_encoding(Kind::Type1, b"/Encoding ISOLatin1Encoding def").is_err());
        assert!(builtin_encoding(Kind::Type1, b"no encoding here").is_err());
        assert!(builtin_encoding(Kind::Type1, b"/Encoding 256 array readonly def").is_err());
        assert!(builtin_encoding(Kind::Type1, b"").is_err());
    }

    #[test]
    fn type1_runaway_encoding_is_bounded() {
        let mut data = b"/Encoding 256 array\n".to_vec();
        for _ in 0..200_000 {
            data.extend_from_slice(b"dup 1 /a put\n");
        }
        let started = std::time::Instant::now();
        let t = builtin_encoding(Kind::Type1, &data).unwrap();
        assert_eq!(show(&t, 1), "a");
        assert!(started.elapsed().as_secs() < 2);
    }

    // --- CFF ---------------------------------------------------------------------------------------

    fn index(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = u16::try_from(items.len()).unwrap().to_be_bytes().to_vec();
        if items.is_empty() {
            return out;
        }
        out.push(2);
        let mut off = 1u16;
        out.extend(off.to_be_bytes());
        for it in items {
            off += u16::try_from(it.len()).unwrap();
            out.extend(off.to_be_bytes());
        }
        for it in items {
            out.extend(it);
        }
        out
    }

    fn int5(v: usize) -> Vec<u8> {
        let mut o = vec![29];
        o.extend(u32::try_from(v).unwrap().to_be_bytes());
        o
    }

    /// A CFF font of `glyphs` glyphs with the given charset and encoding data (`None`: the
    /// predefined ISOAdobe charset, Standard encoding) and extra Top DICT bytes.
    fn build_cff(strings: &[&str], glyphs: usize, charset: Option<Vec<u8>>, encoding: Option<Vec<u8>>, extra_top: Vec<u8>) -> Vec<u8> {
        let header = vec![1, 0, 4, 2];
        let names = index(&[b"T".to_vec()]);
        let strs = index(&strings.iter().map(|s| s.as_bytes().to_vec()).collect::<Vec<_>>());
        let gsubr = index(&[]);
        let top_len = 6 * (1 + usize::from(charset.is_some()) + usize::from(encoding.is_some())) + extra_top.len();
        let pos = header.len() + names.len() + (7 + top_len) + strs.len() + gsubr.len();
        let mut top = extra_top;
        let mut tail = Vec::new();
        if let Some(cs) = &charset {
            top.extend(int5(pos + tail.len()));
            top.push(15);
            tail.extend(cs);
        }
        if let Some(en) = &encoding {
            top.extend(int5(pos + tail.len()));
            top.push(16);
            tail.extend(en);
        }
        top.extend(int5(pos + tail.len()));
        top.push(17);
        tail.extend(index(&vec![vec![14u8]; glyphs]));
        assert_eq!(top.len(), top_len);
        [header, names, index(&[top]), strs, gsubr, tail].concat()
    }

    #[test]
    fn cff_format_0_charset_and_encoding() {
        // Glyphs 1 to 4: A, a, a custom "uni4E2D", a custom "g7".
        let charset = vec![0, 0, 34, 0, 66, 1, 135, 1, 136];
        let encoding = vec![0, 4, 65, 97, 200, 201];
        let font = build_cff(&["uni4E2D", "g7"], 5, Some(charset), Some(encoding), Vec::new());
        let t = builtin_encoding(Kind::Cff, &font).unwrap();
        assert_eq!(shown(&t, &[65, 97, 200, 201, 66, 0]), "A a 中 ? - -");
    }

    #[test]
    fn cff_format_1_ranges_and_supplement() {
        // Glyphs 1 to 3: SIDs 34..=36 (A B C), glyph 4: custom SID 391.
        let charset = vec![1, 0, 34, 2, 1, 135, 0];
        // Codes 0x41..=0x43 are glyphs 1..=3, 0xA0 glyph 4; the supplement gives code 122 the glyph SID 35 (B).
        let encoding = vec![0x81, 2, 0x41, 2, 0xA0, 0, 1, 122, 0, 35];
        let font = build_cff(&["uni4E2D"], 5, Some(charset), Some(encoding), Vec::new());
        let t = builtin_encoding(Kind::Cff, &font).unwrap();
        assert_eq!(shown(&t, &[65, 66, 67, 160, 122, 68]), "A B C 中 B -");
    }

    #[test]
    fn cff_format_2_charset() {
        let charset = vec![2, 0, 34, 0, 3];
        let encoding = vec![0, 4, 1, 2, 3, 4];
        let font = build_cff(&[], 5, Some(charset), Some(encoding), Vec::new());
        let t = builtin_encoding(Kind::Cff, &font).unwrap();
        assert_eq!(shown(&t, &[1, 2, 3, 4, 5]), "A B C D -");
    }

    #[test]
    fn cff_predefined_pieces() {
        // The ISOAdobe charset: glyph i is string i. Codes 65.. are glyphs 1.., so code 98 is glyph 34 (A).
        let font = build_cff(&[], 50, None, Some(vec![1, 1, 65, 40]), Vec::new());
        let t = builtin_encoding(Kind::Cff, &font).unwrap();
        assert_eq!(shown(&t, &[98, 99, 66]), "A B !");
        // Standard encoding: the table of the spec.
        let font = build_cff(&[], 3, None, None, Vec::new());
        assert_eq!(shown(&builtin_encoding(Kind::Cff, &font).unwrap(), &[65, 174]), "A fi");
        // Not supported: the Expert encoding and charsets, CID-keyed fonts.
        let font = build_cff(&[], 3, None, None, [int5(1), vec![16]].concat());
        assert!(builtin_encoding(Kind::Cff, &font).is_err());
        let font = build_cff(&[], 3, None, Some(vec![0, 1, 65]), [int5(1), vec![15]].concat());
        assert!(builtin_encoding(Kind::Cff, &font).is_err());
        let font = build_cff(&[], 3, None, None, vec![139, 139, 139, 12, 30]);
        assert!(builtin_encoding(Kind::Cff, &font).is_err());
        assert!(builtin_encoding(Kind::Cff, b"").is_err());
        assert!(builtin_encoding(Kind::Cff, b"\x02\x00\x04\x01").is_err());
    }

    #[test]
    fn cff_inside_opentype() {
        let font = build_cff(&[], 5, Some(vec![2, 0, 34, 0, 3]), Some(vec![0, 4, 1, 2, 3, 4]), Vec::new());
        let otf = sfnt(&[(b"CFF ", font), (b"head", vec![0; 54])]);
        assert_eq!(shown(&builtin_encoding(Kind::Cff, &otf).unwrap(), &[1, 2]), "A B");
        assert!(builtin_encoding(Kind::Cff, &sfnt(&[(b"head", vec![0; 54])])).is_err());
    }

    // --- TrueType ----------------------------------------------------------------------------------

    fn sfnt(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0, 1, 0, 0];
        out.extend(u16::try_from(tables.len()).unwrap().to_be_bytes());
        out.extend([0u8; 6]);
        let mut off = 12 + 16 * tables.len();
        for (tag, data) in tables {
            out.extend(*tag);
            out.extend([0u8; 4]);
            out.extend(u32::try_from(off).unwrap().to_be_bytes());
            out.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
            off += data.len();
        }
        for (_, data) in tables {
            out.extend(data);
        }
        out
    }

    fn cmap_table(subs: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0, 0];
        out.extend(u16::try_from(subs.len()).unwrap().to_be_bytes());
        let mut off = 4 + 8 * subs.len();
        for (platform, encoding, data) in subs {
            out.extend(platform.to_be_bytes());
            out.extend(encoding.to_be_bytes());
            out.extend(u32::try_from(off).unwrap().to_be_bytes());
            off += data.len();
        }
        for (_, _, data) in subs {
            out.extend(data);
        }
        out
    }

    /// A format 4 subtable: segments (start, end, delta, glyph array) and the closing 0xFFFF one.
    fn format4(segs: &[(u16, u16, u16, Option<Vec<u16>>)]) -> Vec<u8> {
        let sc = segs.len() + 1;
        let mut array: Vec<u16> = Vec::new();
        let mut ros: Vec<u16> = Vec::new();
        for (i, seg) in segs.iter().enumerate() {
            match &seg.3 {
                Some(a) => {
                    ros.push(u16::try_from(2 * (sc - i) + 2 * array.len()).unwrap());
                    array.extend(a);
                }
                None => ros.push(0),
            }
        }
        ros.push(0);
        let mut words: Vec<u16> = vec![4, 0, 0, u16::try_from(2 * sc).unwrap(), 0, 0, 0];
        words.extend(segs.iter().map(|s| s.1));
        words.push(0xffff);
        words.push(0);
        words.extend(segs.iter().map(|s| s.0));
        words.push(0xffff);
        words.extend(segs.iter().map(|s| s.2));
        words.push(1);
        words.extend(ros);
        words.extend(array);
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    fn format0(map: &[(usize, u8)]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 6, 0, 0];
        let mut glyphs = vec![0u8; 256];
        for &(code, gid) in map {
            glyphs[code] = gid;
        }
        out.extend(glyphs);
        out
    }

    fn format12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut out = vec![0, 12, 0, 0];
        out.extend(u32::try_from(16 + 12 * groups.len()).unwrap().to_be_bytes());
        out.extend(0u32.to_be_bytes());
        out.extend(u32::try_from(groups.len()).unwrap().to_be_bytes());
        for g in groups {
            out.extend(g.0.to_be_bytes());
            out.extend(g.1.to_be_bytes());
            out.extend(g.2.to_be_bytes());
        }
        out
    }

    fn post2(indexes: &[u16], custom: &[&str]) -> Vec<u8> {
        let mut out = vec![0, 2, 0, 0];
        out.extend([0u8; 28]);
        out.extend(u16::try_from(indexes.len()).unwrap().to_be_bytes());
        for i in indexes {
            out.extend(i.to_be_bytes());
        }
        for name in custom {
            out.push(u8::try_from(name.len()).unwrap());
            out.extend(name.as_bytes());
        }
        out
    }

    fn glyph_delta(code: u16, gid: u16) -> u16 {
        gid.wrapping_sub(code)
    }

    #[test]
    fn truetype_symbolic_through_the_reversed_unicode_cmap() {
        // (3,0): codes 0xF020..=0xF023 give glyphs 5..=8 through the glyph array; (3,1) maps A..D to them.
        let sym = format4(&[(0xF020, 0xF023, 0, Some(vec![5, 6, 7, 8]))]);
        let uni = format4(&[(0x41, 0x44, glyph_delta(0x41, 5), None)]);
        let font = sfnt(&[(b"cmap", cmap_table(&[(3, 0, sym), (3, 1, uni)]))]);
        let t = builtin_encoding(Kind::TrueType, &font).unwrap();
        assert_eq!(shown(&t, &[0x20, 0x21, 0x22, 0x23, 0x24, 0x41]), "A B C D - -");
    }

    #[test]
    fn truetype_symbolic_through_post_names() {
        // (3,0) codes 0xF041..=0xF044 are glyphs 1..=4; glyph names: Macintosh 36 (A), 68 (a), custom two.
        let sym = format4(&[(0xF041, 0xF044, glyph_delta(0xF041, 1), None)]);
        let post = post2(&[0, 36, 68, 258, 259], &["uni4E2D", "q99"]);
        let font = sfnt(&[(b"cmap", cmap_table(&[(3, 0, sym)])), (b"post", post)]);
        let t = builtin_encoding(Kind::TrueType, &font).unwrap();
        assert_eq!(shown(&t, &[0x41, 0x42, 0x43, 0x44, 0x45]), "A a 中 ? -");
    }

    #[test]
    fn truetype_mac_cmap_and_format_12_unicode() {
        let mac = format0(&[(65, 10), (66, 11), (67, 12)]);
        let uni = format12(&[(0x3042, 0x3043, 10), (0x1F600, 0x1F600, 12)]);
        let font = sfnt(&[(b"cmap", cmap_table(&[(1, 0, mac), (3, 10, uni)]))]);
        let t = builtin_encoding(Kind::TrueType, &font).unwrap();
        // Glyph 12 is outside the BMP in the cmap: its code is U+1F600.
        assert_eq!(shown(&t, &[65, 66, 67, 68]), "あ ぃ \u{1F600} -");
    }

    #[test]
    fn truetype_without_a_usable_cmap() {
        assert!(builtin_encoding(Kind::TrueType, &sfnt(&[(b"head", vec![0; 54])])).is_err());
        let uni = format4(&[(0x41, 0x44, glyph_delta(0x41, 5), None)]);
        assert!(builtin_encoding(Kind::TrueType, &sfnt(&[(b"cmap", cmap_table(&[(3, 1, uni)]))])).is_err());
        // A (3,0) cmap that maps none of the codes: no error, nothing known.
        let sym = format4(&[(0xF100, 0xF1FF, 0, None)]);
        let font = sfnt(&[(b"cmap", cmap_table(&[(3, 0, sym)]))]);
        let t = builtin_encoding(Kind::TrueType, &font).unwrap();
        assert!(t.iter().all(Option::is_some) || t.iter().all(Option::is_none));
    }

    // --- hostile input -----------------------------------------------------------------------------

    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    /// Every prefix, and some thousands of byte flips, of well-formed programs: no panic, no hang.
    #[test]
    fn broken_programs_are_errors_or_nothing_never_panics() {
        let cff_font = build_cff(&["uni4E2D", "g7"], 5, Some(vec![1, 0, 34, 2, 1, 135, 0]), Some(vec![0x81, 2, 0x41, 2, 0xA0, 0, 1, 122, 0, 35]), Vec::new());
        let sym = format4(&[(0xF020, 0xF023, 0, Some(vec![5, 6, 7, 8])), (0xF041, 0xF044, glyph_delta(0xF041, 1), None)]);
        let uni = format4(&[(0x41, 0x44, glyph_delta(0x41, 5), None)]);
        let tt = sfnt(&[
            (b"cmap", cmap_table(&[(3, 0, sym), (3, 1, uni), (1, 0, format0(&[(65, 3)])), (3, 10, format12(&[(0x41, 0x44, 5)]))])),
            (b"post", post2(&[0, 36, 68, 258, 259], &["uni4E2D", "q99"])),
        ]);
        let otf = sfnt(&[(b"CFF ", cff_font.clone())]);
        let samples: Vec<(Kind, Vec<u8>)> = vec![(Kind::Type1, TYPE1.to_vec()), (Kind::Cff, cff_font), (Kind::Cff, otf), (Kind::TrueType, tt)];
        let mut state = 7u64;
        for (kind, font) in &samples {
            assert!(builtin_encoding(*kind, font).is_ok());
            for n in 0..font.len() {
                let _ = builtin_encoding(*kind, &font[..n]);
            }
            for _ in 0..3000 {
                let mut copy = font.clone();
                for _ in 0..=lcg(&mut state) % 4 {
                    let at = usize::try_from(lcg(&mut state)).unwrap() % copy.len();
                    copy[at] = u8::try_from(lcg(&mut state) % 256).unwrap();
                }
                let _ = builtin_encoding(*kind, &copy);
            }
        }
    }

    #[test]
    fn huge_claimed_counts_are_bounded() {
        let started = std::time::Instant::now();
        // A format 4 subtable claiming 32767 segments that all cover 0..=0xFFFE, and a format 12 one with 4 billion groups.
        let mut f4: Vec<u8> = vec![0, 4, 0, 0, 0, 0, 0xff, 0xfe, 0, 0, 0, 0, 0, 0];
        f4.extend(std::iter::repeat_n([0xffu8, 0xfe], 32767).flatten());
        f4.extend([0, 0]);
        f4.extend(std::iter::repeat_n([0u8, 0], 32767).flatten());
        f4.extend(std::iter::repeat_n([0u8, 1], 32767).flatten());
        f4.extend(std::iter::repeat_n([0u8, 0], 32767).flatten());
        let mut f12 = vec![0, 12, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
        f12.extend([0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        let sym = format0(&[(65, 1)]);
        for sub in [f4, f12] {
            let font = sfnt(&[(b"cmap", cmap_table(&[(1, 0, sym.clone()), (3, 1, sub)]))]);
            let _ = builtin_encoding(Kind::TrueType, &font);
        }
        // A CFF INDEX of 65535 objects in a few bytes.
        let _ = builtin_encoding(Kind::Cff, &[1, 0, 4, 2, 0xff, 0xff, 4, 0, 0, 0, 1]);
        assert!(started.elapsed().as_secs() < 3, "took {:?}", started.elapsed());
    }

    // --- in a document -----------------------------------------------------------------------------

    /// The text of a one-page document with one simple font (object 5) whose descriptor (object 6)
    /// points at the font program (object 7).
    fn extract(font: &str, flags: u32, program_key: &str, program_dict: &str, program: &[u8], content: &str) -> (String, Vec<String>) {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>");
        b.stream_obj(4, "", content.as_bytes());
        b.obj(5, font);
        b.obj(6, &format!("<< /Type /FontDescriptor /FontName /Foo /Flags {flags} /{program_key} 7 0 R >>"));
        b.stream_obj(7, program_dict, program);
        let doc = Document::from_bytes(b.finish_classic(8, "/Root 1 0 R")).expect("the test file opens");
        let mut ex = TextExtractor::new(&doc);
        let pages = doc.pages().expect("pages");
        let text = ex.page_text(&pages[0]).expect("text");
        (text, ex.take_warnings())
    }

    #[test]
    fn a_type1_program_supplies_the_base_encoding() {
        let font = |encoding: &str| format!("<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Foo /FontDescriptor 6 0 R {encoding} >>");
        let run = |encoding: &str, program: &[u8]| extract(&font(encoding), 4, "FontFile", "/Length1 100 /Length2 0 /Length3 0", program, "BT /F1 12 Tf (ABC) Tj ET");
        // No /Encoding: the program's own.
        assert_eq!(run("", TYPE1).0, "XY中\n");
        // /Differences on top of it (an /Encoding dictionary without /BaseEncoding).
        assert_eq!(run("/Encoding << /Differences [66 /Z] >>", TYPE1).0, "XZ中\n");
        // An explicit base encoding wins over the program.
        assert_eq!(run("/Encoding /WinAnsiEncoding", TYPE1).0, "ABC\n");
        // A program that cannot be read: what there was before (the Standard set), and one warning.
        let (text, warnings) = run("", b"%!PS-AdobeFont-1.0\n");
        assert_eq!(text, "ABC\n");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("built-in encoding"), "{warnings:?}");
    }

    #[test]
    fn a_cff_program_supplies_the_base_encoding() {
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Foo /FontDescriptor 6 0 R /Encoding << /Differences [67 /uni00E9] >> >>";
        let program = build_cff(&["uni4E2D"], 5, Some(vec![0, 0, 34, 0, 66, 1, 135, 1, 135]), Some(vec![0, 4, 65, 66, 67, 68]), Vec::new());
        let (text, warnings) = extract(font, 4, "FontFile3", "/Subtype /Type1C", &program, "BT /F1 12 Tf (ABCD) Tj ET");
        assert_eq!(text, "Aa\u{e9}中\n");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_symbolic_truetype_is_read_through_its_cmap_a_nonsymbolic_one_is_not() {
        let font = "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Foo /FontDescriptor 6 0 R >>";
        let sym = format4(&[(0xF020, 0xF023, 0, Some(vec![5, 6, 7, 8]))]);
        let uni = format4(&[(0x41, 0x44, glyph_delta(0x41, 5), None)]);
        let program = sfnt(&[(b"cmap", cmap_table(&[(3, 0, sym), (3, 1, uni)]))]);
        let content = "BT /F1 12 Tf (\\040\\041\\042) Tj ET";
        assert_eq!(extract(font, 4, "FontFile2", "/Length1 100", &program, content).0, "ABC\n");
        // Flags 32 (nonsymbolic): the Windows Latin set, as before.
        assert_eq!(extract(font, 32, "FontFile2", "/Length1 100", &program, content).0, "!\"\n");
    }
}
