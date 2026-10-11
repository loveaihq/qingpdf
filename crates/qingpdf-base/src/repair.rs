//! Rebuilding the cross-reference table of a damaged file by scanning it
//! (ISO 32000-1 Annex C.2, last bullet: readers may rebuild a damaged table by
//! scanning all the objects in the file).
//!
//! One forward pass finds every `n g obj` header and every `trailer` keyword.
//! Objects are parsed as they are found and the scan resumes after each one,
//! so the contents of streams (an attached PDF, say) are not mistaken for
//! objects of this file. Later definitions of an object number win, which is
//! what a chain of incremental updates means anyway.

use crate::error::{Error, Result};
use crate::filter::{self, DecodeBudget, MAX_OBJSTM_DECODED};
use crate::lexer::{find_bytes, is_regular, is_whitespace};
use crate::object::{Dict, ObjRef, Object};
use crate::parser::{Parser, PlainHelper};
use crate::security::Security;
use crate::xref::{ObjStm, XrefEntry, XrefTable, strip_stream_only_keys};

/// Why some object stream could not be opened during a rebuild. The objects
/// inside it are missing from the table, and the message says why: the
/// stream uses a filter this layer does not decode (for example
/// `BrotliDecode filter in an object stream`), or it is too big to decode.
#[derive(Debug, Clone, Default)]
pub struct Unopened {
    pub unsupported: Option<String>,
    pub limit: Option<String>,
}

/// The result of a rebuild.
#[derive(Debug)]
pub struct Repaired {
    pub entries: XrefTable,
    pub trailer: Dict,
    pub uses_xref_streams: bool,
    pub unopened: Unopened,
}

#[derive(Debug, Clone, Copy)]
struct Header {
    /// Where the object number starts.
    start: usize,
    num: u32,
    generation: u16,
    /// Just after the `obj` keyword.
    after: usize,
}

const OBJ: &[u8] = b"obj";
const TRAILER: &[u8] = b"trailer";

/// Find the first `n g obj` at or after `from`: object number and generation
/// as digit runs, separated by white space, then the keyword `obj` as a whole
/// word. Whatever precedes the object number is not looked at, so
/// `endobj12 0 obj` still finds object 12.
fn find_header(data: &[u8], from: usize) -> Option<Header> {
    let mut at = from;
    loop {
        let kw = find_bytes(data, OBJ, at)?;
        at = kw + OBJ.len();
        if data.get(at).is_some_and(|&b| is_regular(b)) {
            continue; // `endobj`, `object`, ...
        }
        if let Some(h) = header_ending_at(data, from, kw) {
            return Some(h);
        }
    }
}

/// Look backwards from the `obj` keyword at `kw` for `<num> <gen> `.
fn header_ending_at(data: &[u8], from: usize, kw: usize) -> Option<Header> {
    let byte_before = |i: usize| i.checked_sub(1).and_then(|j| data.get(j)).copied();
    let mut i = kw;
    let end_ws1 = i;
    while i > from && byte_before(i).is_some_and(is_whitespace) {
        i -= 1;
    }
    if i == end_ws1 {
        return None;
    }
    let gen_end = i;
    while i > from && byte_before(i).is_some_and(|b| b.is_ascii_digit()) {
        i -= 1;
    }
    let gen_digits = data.get(i..gen_end)?;
    let end_ws2 = i;
    while i > from && byte_before(i).is_some_and(is_whitespace) {
        i -= 1;
    }
    if gen_digits.is_empty() || i == end_ws2 {
        return None;
    }
    let num_end = i;
    while i > from && byte_before(i).is_some_and(|b| b.is_ascii_digit()) {
        i -= 1;
    }
    let num_digits = data.get(i..num_end)?;
    if num_digits.is_empty() || num_digits.len() > 10 || gen_digits.len() > 5 {
        return None;
    }
    let num = std::str::from_utf8(num_digits).ok()?.parse::<u32>().ok()?;
    let generation = std::str::from_utf8(gen_digits).ok()?.parse::<u16>().ok()?;
    Some(Header { start: i, num, generation, after: kw + OBJ.len() })
}

/// First `trailer` keyword at or after `from` that stands alone.
fn find_trailer(data: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    loop {
        let p = find_bytes(data, TRAILER, at)?;
        at = p + TRAILER.len();
        let before_ok = p == 0 || p.checked_sub(1).and_then(|j| data.get(j)).is_some_and(|&b| !is_regular(b));
        let after_ok = data.get(at).is_none_or(|&b| !is_regular(b));
        if before_ok && after_ok {
            return Some(p);
        }
    }
}

/// Remembers the next header and trailer found so each is searched for only
/// once however the scan cursor moves.
struct Scanner<'a> {
    data: &'a [u8],
    header: Option<Option<Header>>,
    trailer: Option<Option<usize>>,
}

impl<'a> Scanner<'a> {
    fn new(data: &'a [u8]) -> Self {
        Scanner { data, header: None, trailer: None }
    }

    fn next_header(&mut self, cursor: usize) -> Option<Header> {
        match self.header {
            Some(None) => return None,
            Some(Some(h)) if h.start >= cursor => return Some(h),
            _ => {}
        }
        let found = find_header(self.data, cursor);
        self.header = Some(found);
        found
    }

    fn next_trailer(&mut self, cursor: usize) -> Option<usize> {
        match self.trailer {
            Some(None) => return None,
            Some(Some(t)) if t >= cursor => return Some(t),
            _ => {}
        }
        let found = find_trailer(self.data, cursor);
        self.trailer = Some(found);
        found
    }
}

fn type_is(dict: &Dict, name: &str) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(n)) if n == name)
}

/// Scan `data` and rebuild what the cross-reference section and trailer would
/// have said. Linear in the size of the file: the work spent on parses that
/// fail is capped, after which objects are only indexed, not parsed.
pub fn rebuild(data: &[u8]) -> Result<Repaired> {
    rebuild_budgeted(data, &DecodeBudget::default(), None)
}

/// [`rebuild`], charging the decoding of object streams (they are opened to
/// list what is inside) to `decode_budget`. For an encrypted file the object
/// streams are encrypted: with `security` (its key known) they are decrypted
/// first, without it they cannot be read and what is inside them is not found.
pub fn rebuild_budgeted(data: &[u8], decode_budget: &DecodeBudget, security: Option<&Security>) -> Result<Repaired> {
    let helper = PlainHelper::new(data);
    let mut entries = XrefTable::new();
    let mut trailers: Vec<(usize, Dict)> = Vec::new(); // (position in file, dictionary)
    let mut catalogs: Vec<ObjRef> = Vec::new();
    let mut encryption_dicts: Vec<ObjRef> = Vec::new();
    let mut uses_xref_streams = false;
    let mut unopened = Unopened::default();

    let mut scanner = Scanner::new(data);
    let mut cursor = 0usize;
    let mut wasted = 0usize;
    let budget = data.len().saturating_mul(2).saturating_add(1 << 20);

    loop {
        let header = scanner.next_header(cursor);
        let trailer_at = scanner.next_trailer(cursor);
        match (header, trailer_at) {
            (None, None) => break,
            (header, Some(t)) if header.is_none_or(|h| t < h.start) => {
                let after = t + TRAILER.len();
                if wasted > budget {
                    cursor = after; // out of parsing budget: ignore trailers from here on
                    continue;
                }
                let mut parser = Parser::new(data, after);
                match parser.parse_object() {
                    Ok(Object::Dict(d)) => {
                        trailers.push((t, d));
                        cursor = parser.pos().max(after);
                    }
                    _ => {
                        wasted = wasted.saturating_add(parser.pos().saturating_sub(t));
                        cursor = after;
                    }
                }
            }
            (Some(h), _) => {
                if wasted > budget {
                    // Out of parsing budget: index the object without looking inside.
                    entries.insert(h.num, XrefEntry::InUse { offset: h.start as u64, generation: h.generation });
                    cursor = h.after;
                    continue;
                }
                let mut parser = Parser::new(data, h.start);
                match parser.parse_indirect(&helper) {
                    Ok(raw) => {
                        entries.insert(h.num, XrefEntry::InUse { offset: h.start as u64, generation: h.generation });
                        cursor = raw.end.max(h.after);
                        if let Object::Dict(dict) = &raw.object {
                            if is_encryption_dict(dict) {
                                encryption_dicts.push(ObjRef::new(h.num, h.generation));
                            }
                            if type_is(dict, "Catalog") {
                                catalogs.push(ObjRef::new(h.num, h.generation));
                            } else if type_is(dict, "XRef") && raw.stream.is_some() {
                                uses_xref_streams = true;
                                trailers.push((h.start, dict.clone()));
                            } else if type_is(dict, "ObjStm")
                                && let Some(range) = raw.stream.clone()
                            {
                                let sink = ObjStmSink { entries: &mut entries, catalogs: &mut catalogs, unopened: &mut unopened };
                                let whose = ObjRef::new(h.num, h.generation);
                                index_object_stream(data, whose, dict, range, sink, decode_budget, security);
                            }
                        }
                    }
                    Err(_) => {
                        wasted = wasted.saturating_add(parser.pos().saturating_sub(h.start));
                        cursor = h.after;
                    }
                }
            }
            // The first arm took every case with a trailer before the header.
            (None, Some(_)) => break,
        }
    }

    let Some(max_num) = entries.max_num() else {
        return Err(Error::syntax(None, "no PDF objects found in the file"));
    };

    // The newest trailer whose /Root points at an object we found. In an encrypted
    // file scanned without the key, the catalog may sit in an object stream that
    // could not be opened: the trailer's own /Root is believed then, and the scan
    // that has the key (the document makes one) finds the object.
    let keyless_encrypted = security.is_none();
    let root_ok = |d: &Dict| match d.get("Root") {
        Some(Object::Ref(r)) => entries.contains(r.num) || (keyless_encrypted && d.contains_key("Encrypt")),
        _ => false,
    };
    let chosen = trailers.iter().rev().find(|(_, d)| root_ok(d)).map(|(_, d)| d.clone());
    let mut trailer = match chosen {
        Some(d) => d,
        None => {
            // No usable trailer: find the catalog ourselves.
            let catalog =
                catalogs.last().ok_or_else(|| Error::syntax(None, "no trailer and no document catalog found"))?;
            // Keep the newest trailer's other entries (/Encrypt, /ID, /Info).
            let mut base = trailers.last().map(|(_, d)| d.clone()).unwrap_or_default();
            base.set("Root", Object::Ref(*catalog));
            base
        }
    };
    strip_stream_only_keys(&mut trailer);
    trailer.set("Size", Object::Integer(i64::from(max_num) + 1));
    // If the trailer that named the encryption dictionary was lost, the
    // dictionary itself still gives the file away. Missing it would make an
    // encrypted file look plain, and its scrambled streams would be copied
    // around as if they were data.
    if !trailer.contains_key("Encrypt")
        && let Some(encrypt) = encryption_dicts.last()
    {
        trailer.set("Encrypt", Object::Ref(*encrypt));
    }
    Ok(Repaired { entries, trailer, uses_xref_streams, unopened })
}

/// Does this look like an encryption dictionary (7.6.1, Table 20): the
/// standard security handler with its owner and user entries?
fn is_encryption_dict(dict: &Dict) -> bool {
    matches!(dict.get("Filter"), Some(Object::Name(n)) if n == "Standard")
        && matches!(dict.get("O"), Some(Object::String(_)))
        && matches!(dict.get("U"), Some(Object::String(_)))
        && (dict.contains_key("R") || dict.contains_key("V"))
}

/// What opening an object stream adds to: the table, the catalogs found, and
/// the record of what could not be opened.
struct ObjStmSink<'a> {
    entries: &'a mut XrefTable,
    catalogs: &'a mut Vec<ObjRef>,
    unopened: &'a mut Unopened,
}

/// Record the objects held in an object stream (7.5.7) and note any catalog
/// among them. The stream is decoded here only to read its header; it is
/// decoded again, and cached, when an object in it is first needed.
fn index_object_stream(
    data: &[u8],
    stream: ObjRef,
    dict: &Dict,
    range: std::ops::Range<usize>,
    sink: ObjStmSink<'_>,
    budget: &DecodeBudget,
    security: Option<&Security>,
) {
    let ObjStmSink { entries, catalogs, unopened } = sink;
    let stream_num = stream.num;
    let Some(raw) = data.get(range) else {
        return;
    };
    let decrypted;
    let raw = match security {
        Some(security) => match security.decrypt_stream_data(stream, dict, raw.to_vec()) {
            Ok(plain) => {
                decrypted = plain;
                decrypted.as_slice()
            }
            Err(_) => return,
        },
        None => raw,
    };
    let decoded = match filter::decode_with_limit(dict, raw, &|o| Ok(o.clone()), MAX_OBJSTM_DECODED, 0, budget) {
        Ok(d) => d,
        Err(Error::Unsupported(m)) => {
            // The objects inside cannot be found. Remember why, so that a
            // missing page tree is reported as "unsupported", not "damaged".
            unopened.unsupported.get_or_insert_with(|| format!("{m} in an object stream"));
            return;
        }
        // Too big to decode (or the document's decoding budget is spent):
        // say so, so that a page tree inside is reported as a limit hit.
        Err(Error::Limit(m)) => {
            unopened.limit.get_or_insert_with(|| format!("{m} (in an object stream)"));
            return;
        }
        Err(_) => return,
    };
    let Ok(stm) = ObjStm::parse(dict, decoded) else {
        return;
    };
    let mut budget = stm.data().len().saturating_mul(2).saturating_add(4096);
    for (index, (num, offset)) in stm.offsets().enumerate() {
        entries.insert(num, XrefEntry::Compressed { stream_num, index: u32::try_from(index).unwrap_or(u32::MAX) });
        // Only dictionaries can be catalogs, and only a bounded amount of
        // parsing is spent on looking.
        if budget == 0 || !stm.data().get(offset..).is_some_and(starts_with_dict) {
            continue;
        }
        let mut parser = Parser::new(stm.data(), offset);
        let parsed = parser.parse_object();
        budget = budget.saturating_sub(parser.pos().saturating_sub(offset).max(1));
        if let Ok(Object::Dict(d)) = parsed
            && type_is(&d, "Catalog")
        {
            catalogs.push(ObjRef::new(num, 0));
        }
    }
}

fn starts_with_dict(bytes: &[u8]) -> bool {
    let skipped = bytes.iter().position(|&b| !is_whitespace(b)).unwrap_or(bytes.len());
    bytes.get(skipped..).is_some_and(|rest| rest.starts_with(b"<<"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn offset_of(entries: &XrefTable, num: u32) -> usize {
        match entries.get(num) {
            Some(XrefEntry::InUse { offset, .. }) => usize::try_from(offset).unwrap(),
            other => panic!("object {num}: {other:?}"),
        }
    }

    #[test]
    fn rebuilds_a_normal_file_without_using_its_xref() {
        let pdf = sample_pdf();
        let r = rebuild(&pdf).unwrap();
        for num in 1..=4 {
            let at = offset_of(&r.entries, num);
            assert!(pdf[at..].starts_with(format!("{num} 0 obj").as_bytes()));
        }
        assert_eq!(r.entries.len(), 4);
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
        assert_eq!(r.trailer.get_int("Size"), Some(5));
        assert!(!r.uses_xref_streams);
    }

    #[test]
    fn missing_trailer_finds_the_catalog() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(7, "<< /Type /Catalog /Pages 1 0 R >>");
        b.obj(3, "<< /Foo /Bar >>");
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(7, 0))));
        assert_eq!(r.trailer.get_int("Size"), Some(8));
    }

    #[test]
    fn lost_trailer_does_not_hide_encryption() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "<< /Filter /Standard /V 2 /R 3 /Length 128 /P -4 /O <0011> /U (abc) >>");
        // No trailer at all: the encryption dictionary itself is recognised.
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.trailer.get("Encrypt"), Some(&Object::Ref(ObjRef::new(2, 0))));
        // A dictionary that merely mentions /Standard is not one.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "<< /Filter /Standard /V 2 >>");
        b.obj(3, "<< /Filter /Standard /O <00> /U <00> /R 3 /Kind /Other >>");
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.trailer.get("Encrypt"), Some(&Object::Ref(ObjRef::new(3, 0))));
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "<< /Filter /Standard /V 2 >>");
        assert!(!rebuild(&b.finish()).unwrap().trailer.contains_key("Encrypt"));
        // A surviving trailer's own /Encrypt is left alone.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "<< /Filter /Standard /R 3 /O <00> /U <00> >>");
        b.obj(3, "<< /Filter /Standard /R 3 /O <01> /U <01> >>");
        b.raw(b"trailer << /Size 4 /Root 1 0 R /Encrypt 2 0 R >>\n");
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.trailer.get("Encrypt"), Some(&Object::Ref(ObjRef::new(2, 0))));
    }

    #[test]
    fn later_definitions_win() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "(first)");
        let second = b.obj(2, "(second)");
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(offset_of(&r.entries, 2), second);
    }

    #[test]
    fn last_trailer_with_a_valid_root_wins() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "<< /Type /Catalog /Version /1.7 >>");
        b.raw(b"trailer << /Size 3 /Root 1 0 R /Info 9 0 R >>\n");
        b.raw(b"trailer << /Size 3 /Root 2 0 R >>\n");
        // A trailer whose Root does not exist is ignored.
        b.raw(b"trailer << /Size 3 /Root 55 0 R >>\n");
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(2, 0))));
        assert!(!r.trailer.contains_key("Info"));
    }

    #[test]
    fn encrypt_entry_of_the_trailer_survives() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.obj(2, "<< /Filter /Standard /V 1 >>");
        b.raw(b"trailer << /Size 3 /Root 1 0 R /Encrypt 2 0 R >>\n");
        let r = rebuild(&b.finish()).unwrap();
        assert!(r.trailer.contains_key("Encrypt"));
    }

    #[test]
    fn objects_embedded_in_streams_are_not_mistaken_for_ours() {
        // Object 2 is an uncompressed attachment that contains a whole PDF with
        // its own object 1 and a trailer.
        let inner = sample_pdf();
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.stream_obj(2, "/Type /EmbeddedFile", &inner);
        b.raw(b"trailer << /Size 3 /Root 1 0 R >>\n");
        let pdf = b.finish();
        let r = rebuild(&pdf).unwrap();
        assert_eq!(r.entries.len(), 2);
        let at = offset_of(&r.entries, 1);
        assert!(pdf[at..].starts_with(b"1 0 obj\n<< /Type /Catalog >>"));
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
    }

    #[test]
    fn objects_in_object_streams_are_indexed() {
        let mut b = PdfBuilder::new();
        let header = "5 0 6 20 ";
        let body = "<< /Type /Catalog >>  (text)";
        let stm = format!("{header}{body}");
        b.flate_stream_obj(9, &format!("/Type /ObjStm /N 2 /First {}", header.len()), stm.as_bytes());
        b.raw(b"trailer << /Size 10 /Root 5 0 R >>\n");
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.entries.get(5), Some(XrefEntry::Compressed { stream_num: 9, index: 0 }));
        assert_eq!(r.entries.get(6), Some(XrefEntry::Compressed { stream_num: 9, index: 1 }));
        assert!(matches!(r.entries.get(9), Some(XrefEntry::InUse { .. })));
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(5, 0))));
    }

    #[test]
    fn catalog_inside_an_object_stream_without_any_trailer() {
        let mut b = PdfBuilder::new();
        let header = "5 0 6 20 ";
        let stm = format!("{header}<< /Type /Catalog >>  (text)");
        b.flate_stream_obj(9, &format!("/Type /ObjStm /N 2 /First {}", header.len()), stm.as_bytes());
        let r = rebuild(&b.finish()).unwrap();
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(5, 0))));
    }

    #[test]
    fn xref_stream_dictionary_serves_as_trailer() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.stream_obj(2, "/Type /XRef /Size 3 /W [1 2 1] /Root 1 0 R /ID [<aa> <bb>]", &[0u8; 8]);
        let r = rebuild(&b.finish()).unwrap();
        assert!(r.uses_xref_streams);
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
        assert!(r.trailer.contains_key("ID"));
        assert!(!r.trailer.contains_key("W"));
        assert!(!r.trailer.contains_key("Type"));
    }

    #[test]
    fn header_detection_edge_cases() {
        // Compact forms and a missing space before the object number.
        let mut b = PdfBuilder::new();
        b.raw(b"1 0 obj<</Type/Catalog>>endobj2 0 obj(x)endobj\r\n3\t0\r\nobj\n(y)\nendobj\n");
        b.raw(b"4 0 object (not an object)\n5 0 obj");
        let r = rebuild(&b.finish());
        let r = r.unwrap();
        assert!(r.entries.contains(1));
        assert!(r.entries.contains(2));
        assert!(r.entries.contains(3));
        assert!(!r.entries.contains(4));
        // `5 0 obj` with nothing after it: unparseable, not indexed.
        assert!(!r.entries.contains(5));
        // Generation numbers are kept.
        let mut b = PdfBuilder::new();
        b.raw(b"1 0 obj << /Type /Catalog >> endobj\n8 3 obj (g) endobj\n");
        let r = rebuild(&b.finish()).unwrap();
        assert!(matches!(r.entries.get(8), Some(XrefEntry::InUse { generation: 3, .. })));
    }

    #[test]
    fn nothing_to_recover_is_an_error() {
        assert!(rebuild(b"").is_err());
        assert!(rebuild(b"not a pdf at all").is_err());
        assert!(rebuild(b"%PDF-1.4\n1 0 obj (x) endobj\n").is_err()); // objects but no catalog
    }

    #[test]
    fn truncated_stream_at_the_end_is_dropped_not_fatal() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        b.raw(b"2 0 obj\n<< /Length 500 >>\nstream\nabc");
        let r = rebuild(&b.finish()).unwrap();
        assert!(r.entries.contains(1));
        assert!(!r.entries.contains(2));
    }

    #[test]
    fn hostile_unterminated_strings_stay_linear() {
        // 30,000 headers, each followed by an unterminated string that would
        // run to the end of the file: naive rescanning is quadratic.
        let mut data = b"1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        for i in 0..30_000 {
            data.extend_from_slice(format!("{} 0 obj (((((((((((((((((((\n", i + 2).as_bytes());
        }
        let start = std::time::Instant::now();
        let r = rebuild(&data).unwrap();
        assert!(start.elapsed().as_secs() < 10, "took {:?}", start.elapsed());
        assert!(r.entries.contains(1));
        assert_eq!(r.trailer.get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
        // Past the parsing budget the headers are still indexed.
        assert!(r.entries.len() > 1000);
    }

    #[test]
    fn hostile_trailer_keywords_stay_linear() {
        let mut data = b"1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        for _ in 0..30_000 {
            data.extend_from_slice(b"trailer <</A (((((((((((((((((((\n");
        }
        let start = std::time::Instant::now();
        let r = rebuild(&data).unwrap();
        assert!(start.elapsed().as_secs() < 10, "took {:?}", start.elapsed());
        assert!(r.entries.contains(1));
    }

    #[test]
    fn many_objects_are_found_in_order_of_appearance() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        for i in 2..5000u32 {
            b.obj(i, &format!("<< /N {i} >>"));
        }
        let pdf = b.finish();
        let r = rebuild(&pdf).unwrap();
        assert_eq!(r.entries.len(), 4999);
        let at = offset_of(&r.entries, 4321);
        assert!(pdf[at..].starts_with(b"4321 0 obj"));
    }
}
