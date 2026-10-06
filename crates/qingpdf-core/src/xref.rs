//! Cross-reference data: `startxref` (7.5.5), classic tables (7.5.4),
//! cross-reference streams (7.5.8), the `/Prev` chain of incremental updates
//! (7.5.6), hybrid files (7.5.8.4) and object streams (7.5.7).

use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};
use crate::filter;
use crate::lexer::{Lexer, Token, offset_u64, rfind_bytes};
use crate::object::{Dict, Object};
use crate::parser::{Parser, PlainHelper, StreamHelper};

/// Most objects we accept in one file. Annex C, Table C.1 gives 8,388,607 as
/// the architectural limit; a hostile cross-reference stream could otherwise
/// ask for gigabytes of table.
pub const MAX_OBJECTS: usize = 1 << 23;

/// Most objects read from the header of one object stream. Real files hold
/// tens or hundreds per stream.
const MAX_OBJSTM_OBJECTS: u64 = 1 << 20;

/// How far from the end of the file `startxref` is looked for. The spec wants it
/// on the last line; real files carry padding or junk after `%%EOF`.
const STARTXREF_WINDOW: usize = 4096;

/// One cross-reference entry (7.5.4, 7.5.8.3). A free entry and an entry of an
/// unknown type both mean "the null object".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    Free,
    /// An uncompressed object at a byte offset from the start of the file.
    InUse {
        offset: u64,
        generation: u16,
    },
    /// An object inside an object stream (7.5.7); its generation is 0.
    Compressed {
        stream_num: u32,
        index: u32,
    },
}

/// The merged cross-reference information of a whole file.
#[derive(Debug)]
pub struct XrefData {
    pub entries: HashMap<u32, XrefEntry>,
    pub trailer: Dict,
    pub uses_xref_streams: bool,
}

/// Trailer keys carried over from older sections if the newest one lacks them.
const INHERITED_TRAILER_KEYS: [&str; 4] = ["Root", "Info", "Encrypt", "ID"];

/// Keys of a cross-reference stream dictionary that describe the stream itself
/// and do not belong in the trailer (Tables 5 and 17).
const STREAM_ONLY_KEYS: [&str; 7] = ["Type", "W", "Index", "Length", "Filter", "DecodeParms", "DL"];

/// Turn a cross-reference stream dictionary into a plain trailer dictionary.
pub fn strip_stream_only_keys(dict: &mut Dict) {
    for key in STREAM_ONLY_KEYS {
        dict.remove(key);
    }
}

/// The offset after the last `startxref` keyword (7.5.5).
pub fn find_startxref(data: &[u8]) -> Result<usize> {
    let from = data.len().saturating_sub(STARTXREF_WINDOW);
    let pos = rfind_bytes(data, b"startxref", from)
        .ok_or_else(|| Error::syntax(None, "startxref not found near the end of the file"))?;
    let mut lex = Lexer::new(data, pos + "startxref".len());
    match lex.next_token()? {
        Some(Token::Integer(n)) => {
            usize::try_from(n).map_err(|_| Error::syntax(offset_u64(pos), "negative startxref offset"))
        }
        _ => Err(Error::syntax(offset_u64(pos), "startxref is not followed by an offset")),
    }
}

/// Read the whole cross-reference chain of a file: the section `startxref`
/// points to, then (for each section) its `/XRefStm` and its `/Prev`, newest
/// first. The first definition of an object number wins, free entries
/// included, which makes later updates override earlier ones (7.5.6).
pub fn read_xref(data: &[u8]) -> Result<XrefData> {
    let start = find_startxref(data)?;
    let helper = PlainHelper::new(data);
    let mut entries: HashMap<u32, XrefEntry> = HashMap::new();
    let mut trailer: Option<Dict> = None;
    let mut uses_xref_streams = false;
    let mut seen: HashSet<usize> = HashSet::new();
    // (offset, is this a cross-reference stream named by /XRefStm)
    let mut pending: Vec<(usize, bool)> = vec![(start, false)];

    while let Some((offset, from_xrefstm)) = pending.pop() {
        if !seen.insert(offset) {
            return Err(Error::Limit(format!("cross-reference sections form a cycle (offset {offset} seen twice)")));
        }
        let section = read_section(data, offset, &helper)?;
        uses_xref_streams |= section.is_stream || from_xrefstm;
        for (num, entry) in section.entries {
            if !entries.contains_key(&num) {
                if entries.len() >= MAX_OBJECTS {
                    return Err(Error::Limit(format!("more than {MAX_OBJECTS} cross-reference entries")));
                }
                entries.insert(num, entry);
            }
        }
        if !from_xrefstm {
            // Stack order: /XRefStm is searched before /Prev (7.5.8.4), so push
            // /Prev first. A stream named by /XRefStm has no chain of its own.
            if let Some(prev) = offset_entry(&section.trailer, "Prev") {
                pending.push((prev, false));
            }
            if let Some(stm) = offset_entry(&section.trailer, "XRefStm") {
                pending.push((stm, true));
            }
        }
        match trailer.as_mut() {
            None => trailer = Some(section.trailer),
            Some(newest) => {
                for key in INHERITED_TRAILER_KEYS {
                    if !newest.contains_key(key)
                        && let Some(v) = section.trailer.get(key)
                    {
                        newest.set(key, v.clone());
                    }
                }
            }
        }
    }
    let trailer = trailer.ok_or_else(|| Error::syntax(None, "no trailer"))?;
    Ok(XrefData { entries, trailer, uses_xref_streams })
}

/// A byte offset stored in a trailer (`/Prev`, `/XRefStm`). Zero cannot be a
/// real section (it is the file header) and is read as "none".
fn offset_entry(trailer: &Dict, key: &str) -> Option<usize> {
    let n = match trailer.get(key)? {
        Object::Integer(i) => *i,
        Object::Real(r) if r.is_finite() && r.fract() == 0.0 && r.abs() < 1e15 => *r as i64,
        _ => return None,
    };
    usize::try_from(n).ok().filter(|&o| o > 0)
}

struct Section {
    entries: Vec<(u32, XrefEntry)>,
    trailer: Dict,
    is_stream: bool,
}

fn read_section(data: &[u8], offset: usize, helper: &dyn StreamHelper) -> Result<Section> {
    if offset >= data.len() {
        return Err(Error::syntax(offset_u64(offset), "cross-reference offset is past the end of the file"));
    }
    let mut lex = Lexer::new(data, offset);
    match lex.next_token()? {
        Some(Token::Keyword(b"xref")) => read_table(data, lex.pos()),
        Some(Token::Integer(_)) => read_stream_section(data, offset, helper),
        _ => Err(Error::syntax(offset_u64(offset), "no cross-reference section at this offset")),
    }
}

/// A classic table (7.5.4) followed by `trailer` and its dictionary (7.5.5).
///
/// Entries are read as tokens rather than at fixed 20-byte strides, so the
/// usual damage (19- or 21-byte lines, odd spacing) is accepted. The usual
/// off-by-one - a first subsection that says it starts at 1 but whose first
/// entry is the free head `0000000000 65535 f` - is corrected.
fn read_table(data: &[u8], pos: usize) -> Result<Section> {
    let mut lex = Lexer::new(data, pos);
    let mut entries: Vec<(u32, XrefEntry)> = Vec::new();
    let mut first_subsection = true;
    loop {
        let at = lex.pos();
        match lex.next_token()? {
            Some(Token::Keyword(b"trailer")) => break,
            Some(Token::Integer(start)) => {
                let count = match lex.next_token()? {
                    Some(Token::Integer(c)) => c,
                    _ => return Err(Error::syntax(offset_u64(at), "bad cross-reference subsection header")),
                };
                let (Ok(mut start), Ok(count)) = (u64::try_from(start), u64::try_from(count)) else {
                    return Err(Error::syntax(offset_u64(at), "negative cross-reference subsection header"));
                };
                let mut raw: Vec<(u64, u64, bool)> = Vec::new(); // offset, generation, in use
                while (raw.len() as u64) < count {
                    let entry_at = lex.pos();
                    match lex.next_token()? {
                        // The subsection promised more entries than there are.
                        Some(Token::Keyword(b"trailer")) => {
                            lex.set_pos(entry_at);
                            break;
                        }
                        Some(Token::Integer(offset)) => {
                            let generation = match lex.next_token()? {
                                Some(Token::Integer(g)) => g,
                                _ => return Err(Error::syntax(offset_u64(entry_at), "bad cross-reference entry")),
                            };
                            let in_use = match lex.next_token()? {
                                Some(Token::Keyword(b"n")) => true,
                                Some(Token::Keyword(b"f")) => false,
                                _ => return Err(Error::syntax(offset_u64(entry_at), "bad cross-reference entry")),
                            };
                            let (Ok(offset), Ok(generation)) = (u64::try_from(offset), u64::try_from(generation))
                            else {
                                return Err(Error::syntax(
                                    offset_u64(entry_at),
                                    "negative number in cross-reference entry",
                                ));
                            };
                            raw.push((offset, generation, in_use));
                            if raw.len() > MAX_OBJECTS {
                                return Err(Error::Limit(format!("more than {MAX_OBJECTS} cross-reference entries")));
                            }
                        }
                        _ => return Err(Error::syntax(offset_u64(entry_at), "bad cross-reference entry")),
                    }
                }
                if first_subsection && start == 1 && matches!(raw.first(), Some(&(_, 65535, false))) {
                    start = 0;
                }
                first_subsection = false;
                for (i, (offset, generation, in_use)) in raw.into_iter().enumerate() {
                    let Some(num) =
                        u64::try_from(i).ok().and_then(|i| start.checked_add(i)).and_then(|n| u32::try_from(n).ok())
                    else {
                        continue;
                    };
                    let entry = if in_use && offset > 0 {
                        XrefEntry::InUse { offset, generation: u16::try_from(generation).unwrap_or(u16::MAX) }
                    } else {
                        XrefEntry::Free
                    };
                    entries.push((num, entry));
                }
            }
            _ => return Err(Error::syntax(offset_u64(at), "bad cross-reference table")),
        }
    }
    match Parser::new(data, lex.pos()).parse_object()? {
        Object::Dict(trailer) => Ok(Section { entries, trailer, is_stream: false }),
        _ => Err(Error::syntax(offset_u64(lex.pos()), "trailer is not a dictionary")),
    }
}

/// A cross-reference stream object (7.5.8): its dictionary doubles as the
/// trailer.
fn read_stream_section(data: &[u8], offset: usize, helper: &dyn StreamHelper) -> Result<Section> {
    let raw = Parser::new(data, offset).parse_indirect(helper)?;
    let (Object::Dict(mut dict), Some(range)) = (raw.object, raw.stream) else {
        return Err(Error::syntax(offset_u64(offset), "cross-reference section is not a stream"));
    };
    if let Some(ty) = dict.get("Type")
        && !matches!(ty, Object::Name(n) if n == "XRef")
    {
        return Err(Error::syntax(offset_u64(offset), "stream is not a cross-reference stream"));
    }
    let bytes = data.get(range).unwrap_or(&[]);
    let decoded = filter::decode_direct(&dict, bytes)?;
    let entries = parse_xref_stream_entries(&dict, &decoded, offset)?;
    strip_stream_only_keys(&mut dict);
    Ok(Section { entries, trailer: dict, is_stream: true })
}

fn be_value(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// The entries in the decoded data of a cross-reference stream (7.5.8.2,
/// 7.5.8.3). A truncated stream yields the entries that are complete.
fn parse_xref_stream_entries(dict: &Dict, decoded: &[u8], offset: usize) -> Result<Vec<(u32, XrefEntry)>> {
    let bad = |what: &str| Error::syntax(offset_u64(offset), format!("cross-reference stream: {what}"));
    let w = dict.get("W").and_then(Object::as_array).ok_or_else(|| bad("missing /W"))?;
    let mut widths = [0usize; 3];
    for (slot, item) in widths.iter_mut().zip(w.iter()) {
        let n = item.as_int().ok_or_else(|| bad("/W holds a non-integer"))?;
        *slot = usize::try_from(n).ok().filter(|&n| n <= 8).ok_or_else(|| bad("/W field wider than 8 bytes"))?;
    }
    if w.len() < 3 {
        return Err(bad("/W needs three entries"));
    }
    let [w0, w1, w2] = widths;
    let row_len = w0 + w1 + w2;
    if row_len == 0 {
        return Err(bad("/W is all zeros"));
    }

    // /Index defaults to [0 Size] (Table 17).
    let mut subsections: Vec<(u64, u64)> = Vec::new();
    match dict.get("Index") {
        Some(Object::Array(items)) => {
            // A stray last element without a partner is ignored.
            for [a, b] in items.as_chunks::<2>().0 {
                let (Some(first), Some(count)) = (a.as_int(), b.as_int()) else {
                    return Err(bad("/Index holds a non-integer"));
                };
                let (Ok(first), Ok(count)) = (u64::try_from(first), u64::try_from(count)) else {
                    return Err(bad("negative number in /Index"));
                };
                subsections.push((first, count));
            }
        }
        _ => {
            let size = dict.get_int("Size").unwrap_or(0).max(0);
            subsections.push((0, u64::try_from(size).unwrap_or(0)));
        }
    }

    let mut rows = decoded.chunks_exact(row_len);
    let mut entries = Vec::new();
    'sections: for (first, count) in subsections {
        for k in 0..count {
            let Some(row) = rows.next() else {
                break 'sections;
            };
            let Some(num) = first.checked_add(k).and_then(|n| u32::try_from(n).ok()) else {
                continue;
            };
            let Some((f0, rest)) = row.split_at_checked(w0) else { continue };
            let Some((f1, f2)) = rest.split_at_checked(w1) else { continue };
            // A zero-width type field means type 1 (7.5.8.2).
            let kind = if w0 == 0 { 1 } else { be_value(f0) };
            let second = be_value(f1);
            let third = be_value(f2);
            let entry = match kind {
                1 if second > 0 => {
                    XrefEntry::InUse { offset: second, generation: u16::try_from(third).unwrap_or(u16::MAX) }
                }
                2 => match u32::try_from(second) {
                    Ok(stream_num) => {
                        XrefEntry::Compressed { stream_num, index: u32::try_from(third).unwrap_or(u32::MAX) }
                    }
                    Err(_) => XrefEntry::Free,
                },
                // Type 0, offset 0, and unknown types all read as the null
                // object (7.5.8.3).
                _ => XrefEntry::Free,
            };
            entries.push((num, entry));
            if entries.len() > MAX_OBJECTS {
                return Err(Error::Limit(format!("more than {MAX_OBJECTS} cross-reference entries")));
            }
        }
    }
    Ok(entries)
}

/// A decoded object stream (7.5.7): the object numbers and where each object
/// starts. The objects themselves are parsed on demand.
pub struct ObjStm {
    data: Vec<u8>,
    /// (object number, absolute offset into `data`), in stream order.
    pairs: Vec<(u32, usize)>,
    /// First position in `pairs` of each object number.
    by_num: HashMap<u32, usize>,
}

impl ObjStm {
    /// `data` is the decoded stream data, `dict` its dictionary (`/N`, `/First`).
    pub fn parse(dict: &Dict, data: Vec<u8>) -> Result<ObjStm> {
        let n = dict
            .get_int("N")
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| Error::syntax(None, "object stream without a valid /N"))?;
        let first = dict
            .get_int("First")
            .and_then(|f| usize::try_from(f).ok())
            .filter(|&f| f <= data.len())
            .ok_or_else(|| Error::syntax(None, "object stream without a valid /First"))?;
        let header = data.get(..first).unwrap_or(&[]);
        let mut lex = Lexer::new(header, 0);
        let mut pairs: Vec<(u32, usize)> = Vec::new();
        let mut by_num: HashMap<u32, usize> = HashMap::new();
        // Stops at the first malformed or missing pair; `n` may be absurdly large.
        for _ in 0..n.min(MAX_OBJSTM_OBJECTS) {
            let num = lex.next_token().ok().flatten();
            let off = lex.next_token().ok().flatten();
            let (Some(Token::Integer(num)), Some(Token::Integer(off))) = (num, off) else {
                break;
            };
            let (Ok(num), Ok(off)) = (u32::try_from(num), usize::try_from(off)) else {
                break;
            };
            let Some(abs) = first.checked_add(off).filter(|&a| a < data.len()) else {
                break;
            };
            by_num.entry(num).or_insert(pairs.len());
            pairs.push((num, abs));
        }
        Ok(ObjStm { data, pairs, by_num })
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Object numbers in stream order.
    pub fn object_numbers(&self) -> impl Iterator<Item = u32> + '_ {
        self.pairs.iter().map(|&(n, _)| n)
    }

    /// The decoded data, for callers that need to parse objects themselves.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Start offset of each object, in stream order.
    pub fn offsets(&self) -> impl Iterator<Item = (u32, usize)> + '_ {
        self.pairs.iter().copied()
    }

    /// Object number `num`; `index` is the position the cross-reference entry
    /// claims and is only a hint. `None` if the stream does not contain it.
    pub fn get(&self, num: u32, index: u32) -> Result<Option<Object>> {
        let hinted = usize::try_from(index).ok().and_then(|i| self.pairs.get(i)).filter(|&&(n, _)| n == num);
        let located = hinted.or_else(|| self.by_num.get(&num).and_then(|&i| self.pairs.get(i)));
        let Some(&(_, offset)) = located else {
            return Ok(None);
        };
        Ok(Some(Parser::new(&self.data, offset).parse_object()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Name, ObjRef};
    use crate::testutil::*;

    fn entry_of(x: &XrefData, num: u32) -> XrefEntry {
        *x.entries.get(&num).unwrap_or_else(|| panic!("no entry for {num}"))
    }

    #[test]
    fn classic_table() {
        let pdf = sample_pdf();
        let x = read_xref(&pdf).unwrap();
        assert!(!x.uses_xref_streams);
        assert_eq!(entry_of(&x, 0), XrefEntry::Free);
        for num in 1..=4 {
            let XrefEntry::InUse { offset, generation } = entry_of(&x, num) else {
                panic!("object {num} should be in use")
            };
            assert_eq!(generation, 0);
            let at = usize::try_from(offset).unwrap();
            assert!(pdf[at..].starts_with(format!("{num} 0 obj").as_bytes()), "object {num}");
        }
        assert_eq!(x.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
        assert_eq!(x.trailer.get_int("Size"), Some(5));
    }

    #[test]
    fn startxref_tolerates_junk_after_eof() {
        let mut pdf = sample_pdf();
        pdf.extend_from_slice(&[0u8; 700]);
        pdf.extend_from_slice(b"\r\n\r\ngarbage garbage\n");
        assert!(read_xref(&pdf).is_ok());
        // Without startxref at all.
        assert!(find_startxref(b"%PDF-1.4\n").is_err());
        // Malformed value.
        assert!(find_startxref(b"startxref\nabc\n%%EOF").is_err());
        assert!(find_startxref(b"startxref\n-5\n%%EOF").is_err());
        assert_eq!(find_startxref(b"startxref\n  123 \n%%EOF").unwrap(), 123);
    }

    #[test]
    fn table_with_odd_whitespace_and_short_lines() {
        // 19-byte entries (LF only), extra blank lines and spaces.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o2 = b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let xref = b.len();
        b.raw(b"xref\n0 3\n");
        b.raw(b"0000000000 65535 f\n");
        b.raw(format!("{o1:010} 00000 n\n").as_bytes());
        b.raw(b"\n");
        b.raw(format!("{o2:010}   00000   n  \n").as_bytes());
        b.raw(b"trailer<</Size 3/Root 1 0 R>>\n");
        b.startxref(xref);
        let x = read_xref(&b.finish()).unwrap();
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { .. }));
        assert!(matches!(entry_of(&x, 2), XrefEntry::InUse { .. }));
    }

    #[test]
    fn table_off_by_one_first_subsection() {
        // "1 3" but the first entry is the free head: really starts at 0.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let o2 = b.obj(2, "<< >>");
        let xref = b.len();
        b.raw(b"xref\n1 3\n0000000000 65535 f \n");
        b.raw(format!("{o1:010} 00000 n \n{o2:010} 00000 n \n").as_bytes());
        b.raw(b"trailer << /Size 3 /Root 1 0 R >>\n");
        b.startxref(xref);
        let x = read_xref(&b.finish()).unwrap();
        assert_eq!(entry_of(&x, 0), XrefEntry::Free);
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { offset, .. } if offset == o1 as u64));
        assert!(matches!(entry_of(&x, 2), XrefEntry::InUse { offset, .. } if offset == o2 as u64));
    }

    #[test]
    fn subsection_count_larger_than_entries() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let xref = b.len();
        b.raw(b"xref\n0 50\n0000000000 65535 f \n");
        b.raw(format!("{o1:010} 00000 n \n").as_bytes());
        b.raw(b"trailer << /Size 2 /Root 1 0 R >>\n");
        b.startxref(xref);
        let x = read_xref(&b.finish()).unwrap();
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { .. }));
        assert!(!x.entries.contains_key(&2));
    }

    #[test]
    fn multiple_subsections_and_free_entries() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let o5 = b.obj(5, "<< >>");
        let xref = b.len();
        b.raw(b"xref\n0 2\n0000000000 65535 f \n");
        b.raw(format!("{o1:010} 00000 n \n").as_bytes());
        b.raw(b"3 1\n0000000000 00007 f \n");
        b.raw(format!("5 1\n{o5:010} 00002 n \n").as_bytes());
        b.raw(b"trailer << /Size 6 /Root 1 0 R >>\n");
        b.startxref(xref);
        let x = read_xref(&b.finish()).unwrap();
        assert_eq!(entry_of(&x, 3), XrefEntry::Free);
        assert!(matches!(entry_of(&x, 5), XrefEntry::InUse { generation: 2, .. }));
    }

    #[test]
    fn incremental_update_two_sections() {
        // Original: objects 1-4. Update: replaces object 4, adds 5, frees 3.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o2 = b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        let o3 = b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        let o4 = b.obj(4, "(old)");
        let x1 = b.len();
        b.raw(b"xref\n0 5\n0000000000 65535 f \n");
        for o in [o1, o2, o3, o4] {
            b.raw(format!("{o:010} 00000 n \n").as_bytes());
        }
        b.raw(b"trailer << /Size 5 /Root 1 0 R /Info 4 0 R >>\n");
        b.startxref(x1);

        let o4b = b.obj(4, "(new)");
        let o5 = b.obj(5, "(added)");
        let x2 = b.len();
        b.raw(format!("xref\n3 3\n0000000000 00001 f \n{o4b:010} 00000 n \n{o5:010} 00000 n \n").as_bytes());
        b.raw(format!("trailer << /Size 6 /Root 1 0 R /Prev {x1} >>\n").as_bytes());
        b.startxref(x2);

        let x = read_xref(&b.finish()).unwrap();
        // Newest wins.
        assert_eq!(entry_of(&x, 3), XrefEntry::Free);
        assert!(matches!(entry_of(&x, 4), XrefEntry::InUse { offset, .. } if offset == o4b as u64));
        assert!(matches!(entry_of(&x, 5), XrefEntry::InUse { offset, .. } if offset == o5 as u64));
        // Untouched objects come from the original section.
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { offset, .. } if offset == o1 as u64));
        assert!(matches!(entry_of(&x, 2), XrefEntry::InUse { offset, .. } if offset == o2 as u64));
        // Newest trailer wins, missing keys come from older ones.
        assert_eq!(x.trailer.get_int("Size"), Some(6));
        assert_eq!(x.trailer.get("Info"), Some(&Object::Ref(ObjRef::new(4, 0))));
        assert_eq!(x.trailer.get_int("Prev"), Some(i64::try_from(x1).unwrap()));
        let _ = o3;
    }

    #[test]
    fn prev_cycle_is_detected() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let x1 = b.len();
        // The section's /Prev points at itself.
        b.raw(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev {x1} >>\n"
            )
            .as_bytes(),
        );
        b.startxref(x1);
        assert!(matches!(read_xref(&b.finish()), Err(Error::Limit(_))));

        // Two sections pointing at each other.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let a = b.len();
        let section_len = format!(
            "xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev {a:010} >>\n"
        )
        .len();
        let c = a + section_len;
        b.raw(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev {c:010} >>\n"
            )
            .as_bytes(),
        );
        assert_eq!(b.len(), c);
        b.raw(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev {a:010} >>\n"
            )
            .as_bytes(),
        );
        b.startxref(c);
        assert!(matches!(read_xref(&b.finish()), Err(Error::Limit(_))));
    }

    #[test]
    fn prev_pointing_at_garbage_is_an_error() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let x1 = b.len();
        b.raw(
            format!("xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev 9 >>\n")
                .as_bytes(),
        );
        b.startxref(x1);
        assert!(read_xref(&b.finish()).is_err());
        // Beyond the end of the file.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let x1 = b.len();
        b.raw(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev 99999999 >>\n"
            )
            .as_bytes(),
        );
        b.startxref(x1);
        assert!(read_xref(&b.finish()).is_err());
    }

    #[test]
    fn cross_reference_stream_with_png_predictor() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o2 = b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        // The xref stream is object 3 and lists objects 0-3.
        let x_off = b.len();
        let to_row = |t: u8, off: usize, g: u8| [t, (off >> 8) as u8, off as u8, g];
        let rows = [to_row(0, 0, 255), to_row(1, o1, 0), to_row(1, o2, 0), to_row(1, x_off, 0)];
        let packed = png_up_flate(&rows);
        b.stream_obj(
            3,
            "/Type /XRef /Size 4 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>",
            &packed,
        );
        b.startxref(x_off);
        let x = read_xref(&b.finish()).unwrap();
        assert!(x.uses_xref_streams);
        assert_eq!(entry_of(&x, 0), XrefEntry::Free);
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { offset, .. } if offset == o1 as u64));
        assert!(matches!(entry_of(&x, 2), XrefEntry::InUse { offset, .. } if offset == o2 as u64));
        assert!(matches!(entry_of(&x, 3), XrefEntry::InUse { offset, .. } if offset == x_off as u64));
        // The trailer is the stream dictionary without the stream-only keys.
        assert_eq!(x.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
        assert_eq!(x.trailer.get_int("Size"), Some(4));
        for key in ["Type", "W", "Length", "Filter", "DecodeParms", "Index"] {
            assert!(!x.trailer.contains_key(key), "{key} should be stripped");
        }
    }

    #[test]
    fn cross_reference_stream_index_and_types() {
        // Uncompressed, /Index with two subsections, a compressed entry, an
        // unknown type, and a zero-width type field in a second stream.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let x_off = b.len();
        let mut data = Vec::new();
        let row = |t: u8, f2: u16, f3: u8| [t, (f2 >> 8) as u8, f2 as u8, f3];
        data.extend_from_slice(&row(1, o1 as u16, 0)); // object 1
        data.extend_from_slice(&row(2, 20, 3)); // object 7: in objstm 20, index 3
        data.extend_from_slice(&row(9, 5, 5)); // object 8: unknown type -> free
        data.extend_from_slice(&row(1, x_off as u16, 4)); // object 9: the xref stream, gen 4
        b.stream_obj(9, "/Type /XRef /Size 10 /W [1 2 1] /Index [1 1 7 3] /Root 1 0 R", &data);
        b.startxref(x_off);
        let x = read_xref(&b.finish()).unwrap();
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { .. }));
        assert_eq!(entry_of(&x, 7), XrefEntry::Compressed { stream_num: 20, index: 3 });
        assert_eq!(entry_of(&x, 8), XrefEntry::Free);
        assert!(matches!(entry_of(&x, 9), XrefEntry::InUse { generation: 4, .. }));
        assert!(!x.entries.contains_key(&0));
    }

    #[test]
    fn cross_reference_stream_zero_width_type_field() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let x_off = b.len();
        // W [0 2 0]: no type (defaults to 1), 2-byte offset, no generation.
        let mut data = vec![0u8, 0];
        data.extend_from_slice(&[(o1 >> 8) as u8, o1 as u8]);
        b.stream_obj(2, "/Type /XRef /Size 3 /W [0 2 0] /Root 1 0 R", &data);
        let _ = x_off;
        b.startxref(x_off);
        let x = read_xref(&b.finish()).unwrap();
        // First entry has offset 0 -> free; second is in use.
        assert_eq!(entry_of(&x, 0), XrefEntry::Free);
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { generation: 0, .. }));
    }

    #[test]
    fn cross_reference_stream_bad_dictionaries() {
        for dict in [
            "/Type /XRef /Size 3 /Root 1 0 R",                                // no /W
            "/Type /XRef /Size 3 /W [1 2] /Root 1 0 R",                       // two fields
            "/Type /XRef /Size 3 /W [0 0 0] /Root 1 0 R",                     // zero-width rows
            "/Type /XRef /Size 3 /W [1 9 1] /Root 1 0 R",                     // field too wide
            "/Type /XRef /Size 3 /W [1 -1 1] /Root 1 0 R",                    // negative
            "/Type /XRef /Size 3 /W [1 2 1] /Index [0] /Root 1 0 R /Foo (x)", // odd /Index: ignored pair, not fatal
            "/Type /Catalog /Size 3 /W [1 2 1] /Root 1 0 R",                  // wrong type
        ] {
            let mut b = PdfBuilder::new();
            b.obj(1, "<< /Type /Catalog >>");
            let x_off = b.len();
            b.stream_obj(2, dict, &[0, 0, 0, 0]);
            b.startxref(x_off);
            let r = read_xref(&b.finish());
            if dict.contains("/Index [0]") {
                assert!(r.is_ok(), "{dict}");
            } else {
                assert!(r.is_err(), "{dict}: {r:?}");
            }
        }
    }

    #[test]
    fn cross_reference_stream_with_absurd_size_stops_at_data() {
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let x_off = b.len();
        let data = [1u8, (o1 >> 8) as u8, o1 as u8];
        b.stream_obj(2, "/Type /XRef /Size 2147483647 /W [1 2 0] /Index [1 2147483647] /Root 1 0 R", &data);
        b.startxref(x_off);
        let x = read_xref(&b.finish()).unwrap();
        assert_eq!(x.entries.len(), 1);
    }

    #[test]
    fn hybrid_file() {
        // A classic table (object 3 listed as free) plus an /XRefStm stream that
        // lists object 3 as compressed; the stream wins over the older free entry.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>");
        let o2 = b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        // Main (older) table: objects 0-3, with 3 free.
        let main = b.len();
        b.raw(b"xref\n0 4\n0000000000 65535 f \n");
        b.raw(format!("{o1:010} 00000 n \n{o2:010} 00000 n \n0000000000 65535 f \n").as_bytes());
        b.raw(b"trailer << /Size 5 /Root 1 0 R >>\n");
        b.startxref(main);
        // The xref stream (object 4) says: object 3 is compressed in stream 9, index 0.
        let stm = b.len();
        let data = [2u8, 0, 9, 0];
        b.stream_obj(4, "/Type /XRef /Size 5 /W [1 2 1] /Index [3 1]", &data);
        // New section with an empty table naming the stream.
        let upd = b.len();
        b.raw(format!("xref\n0 0\ntrailer << /Size 5 /Root 1 0 R /Prev {main} /XRefStm {stm} >>\n").as_bytes());
        b.startxref(upd);
        let x = read_xref(&b.finish()).unwrap();
        assert!(x.uses_xref_streams);
        assert_eq!(entry_of(&x, 3), XrefEntry::Compressed { stream_num: 9, index: 0 });
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { .. }));
        assert_eq!(x.trailer.get_int("XRefStm"), Some(i64::try_from(stm).unwrap()));
    }

    #[test]
    fn hybrid_table_entry_in_same_section_wins_over_xrefstm() {
        // Per 7.5.8.4 the stream is consulted only when the table has no entry.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog >>");
        let stm = b.len();
        b.stream_obj(4, "/Type /XRef /Size 5 /W [1 2 1] /Index [1 1]", &[2u8, 0, 9, 0]);
        let main = b.len();
        b.raw(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer << /Size 5 /Root 1 0 R /XRefStm {stm} >>\n"
            )
            .as_bytes(),
        );
        b.startxref(main);
        let x = read_xref(&b.finish()).unwrap();
        assert!(matches!(entry_of(&x, 1), XrefEntry::InUse { .. }));
    }

    #[test]
    fn many_sections_chain() {
        // 300 incremental updates, each redefining object 1.
        let mut b = PdfBuilder::new();
        b.obj(1, "(v0)");
        let mut prev = b.len();
        let o = b.offset_of(1);
        b.raw(
            format!("xref\n0 2\n0000000000 65535 f \n{o:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R >>\n").as_bytes(),
        );
        let mut last = prev;
        for i in 1..300 {
            let o = b.obj(1, &format!("(v{i})"));
            last = b.len();
            b.raw(format!("xref\n1 1\n{o:010} 00000 n \ntrailer << /Size 2 /Root 1 0 R /Prev {prev} >>\n").as_bytes());
            prev = last;
        }
        b.startxref(last);
        let pdf = b.finish();
        let x = read_xref(&pdf).unwrap();
        let XrefEntry::InUse { offset, .. } = entry_of(&x, 1) else { panic!() };
        assert!(pdf[usize::try_from(offset).unwrap()..].starts_with(b"1 0 obj\n(v299)"));
    }

    #[test]
    fn xref_pointing_at_nothing() {
        assert!(read_xref(b"%PDF-1.4\nstartxref\n5\n%%EOF").is_err());
        assert!(read_xref(b"%PDF-1.4\nstartxref\n0\n%%EOF").is_err());
        assert!(read_xref(b"%PDF-1.4\nstartxref\n99999\n%%EOF").is_err());
        assert!(read_xref(b"").is_err());
    }

    #[test]
    fn table_garbage_is_an_error_not_a_panic() {
        for body in [
            "xref\n0 1\n",
            "xref\n0\n",
            "xref\nabc\n",
            "xref\n0 1\n0000000000 65535 x \ntrailer << >>",
            "xref\n0 1\n0000000000 65535 f \ntrailer 5",
            "xref\n-1 2\n",
            "xref\n0 -2\n",
            "xref\n0 1\n0000000000 -1 f \ntrailer << >>",
            "xref\n0 1\n0000000000 65535 f \ntrailer",
        ] {
            let mut pdf = b"%PDF-1.4\n".to_vec();
            let off = pdf.len();
            pdf.extend_from_slice(body.as_bytes());
            pdf.extend_from_slice(format!("\nstartxref\n{off}\n%%EOF").as_bytes());
            assert!(read_xref(&pdf).is_err(), "{body}");
        }
    }

    // --- object streams -------------------------------------------------------

    fn objstm_dict(n: i64, first: i64) -> Dict {
        let mut d = Dict::new();
        d.set("Type", Object::from("ObjStm"));
        d.set("N", Object::Integer(n));
        d.set("First", Object::Integer(first));
        d
    }

    #[test]
    fn object_stream_lookup() {
        // Objects start at 0, 11 and 19 of the body (relative to /First).
        let header = "11 0 12 11 13 19 ";
        let body = "<< /A 1 >> (hello) /Name";
        let data = format!("{header}{body}").into_bytes();
        let s = ObjStm::parse(&objstm_dict(3, i64::try_from(header.len()).unwrap()), data).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s.object_numbers().collect::<Vec<_>>(), vec![11, 12, 13]);
        let a = s.get(11, 0).unwrap().unwrap();
        assert_eq!(a.as_dict().unwrap().get_int("A"), Some(1));
        assert!(matches!(s.get(12, 1).unwrap().unwrap(), Object::String(_)));
        assert_eq!(s.get(13, 2).unwrap().unwrap(), Object::Name(Name::from("Name")));
        // A wrong index hint still finds the object by number.
        assert_eq!(s.get(13, 0).unwrap().unwrap(), Object::Name(Name::from("Name")));
        assert_eq!(s.get(13, 999).unwrap().unwrap(), Object::Name(Name::from("Name")));
        // Not in the stream.
        assert_eq!(s.get(99, 0).unwrap(), None);
    }

    #[test]
    fn object_stream_bad_headers() {
        assert!(ObjStm::parse(&Dict::new(), b"1 0 x".to_vec()).is_err());
        assert!(ObjStm::parse(&objstm_dict(1, 99), b"1 0 x".to_vec()).is_err());
        assert!(ObjStm::parse(&objstm_dict(-1, 0), b"".to_vec()).is_err());
        // /N larger than the real pair count, absurd /N, offsets past the end.
        let s = ObjStm::parse(&objstm_dict(5, 4), b"1 0 (a)".to_vec()).unwrap();
        assert_eq!(s.len(), 1);
        let s = ObjStm::parse(&objstm_dict(i64::MAX, 4), b"1 0 (a)".to_vec()).unwrap();
        assert_eq!(s.len(), 1);
        let s = ObjStm::parse(&objstm_dict(1, 4), b"1 99 (a)".to_vec()).unwrap();
        assert_eq!(s.len(), 0);
    }
}
