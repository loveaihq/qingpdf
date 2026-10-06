//! Object parser (ISO 32000-1 7.3): direct objects, `n g obj ... endobj`, and
//! stream extents (7.3.8.2).

use std::cell::OnceCell;
use std::ops::Range;

use crate::error::{Error, Result};
use crate::lexer::{Lexer, Token, find_bytes, is_whitespace, offset_u64};
use crate::object::{Dict, MAX_OBJECT_NUMBER, Name, ObjRef, Object, Stream};

/// Deepest allowed nesting of arrays and dictionaries.
pub const MAX_NESTING: usize = 256;

/// What the parser needs from its environment to find where a stream ends.
pub trait StreamHelper {
    /// Value of an indirect `/Length` (7.3.8.2), or `None` if it cannot be
    /// resolved to an integer.
    fn resolve_length(&self, r: ObjRef) -> Option<i64>;
    /// Position of the first `endstream` keyword at or after `from`, used when
    /// `/Length` is missing or wrong.
    fn find_endstream(&self, from: usize) -> Option<usize>;
}

/// All `endstream` positions of one file, found in a single linear pass the
/// first time they are needed. Looking up the next one is then a binary
/// search, so files full of broken streams cannot make the parser quadratic.
///
/// Always call [`EndstreamIndex::find`] with the same `data`.
#[derive(Default)]
pub struct EndstreamIndex {
    positions: OnceCell<Vec<usize>>,
}

const ENDSTREAM: &[u8] = b"endstream";

impl EndstreamIndex {
    pub fn find(&self, data: &[u8], from: usize) -> Option<usize> {
        let positions = self.positions.get_or_init(|| {
            let mut out = Vec::new();
            let mut at = 0usize;
            while let Some(p) = find_bytes(data, ENDSTREAM, at) {
                out.push(p);
                at = p.saturating_add(ENDSTREAM.len());
            }
            out
        });
        let i = positions.partition_point(|&p| p < from);
        positions.get(i).copied()
    }
}

/// A [`StreamHelper`] that cannot resolve indirect lengths. Used while the
/// cross-reference data is still being read, and by the repair scan.
pub struct PlainHelper<'a> {
    data: &'a [u8],
    index: EndstreamIndex,
}

impl<'a> PlainHelper<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        PlainHelper { data, index: EndstreamIndex::default() }
    }
}

impl StreamHelper for PlainHelper<'_> {
    fn resolve_length(&self, _r: ObjRef) -> Option<i64> {
        None
    }

    fn find_endstream(&self, from: usize) -> Option<usize> {
        self.index.find(self.data, from)
    }
}

/// An indirect object as it sits in the file. For a stream object, `object` is
/// the stream dictionary and `stream` is the range of its raw data in the
/// file; nothing is copied until [`RawObject::into_object`].
#[derive(Debug)]
pub struct RawObject {
    pub obj_ref: ObjRef,
    pub object: Object,
    pub stream: Option<Range<usize>>,
    /// Position just after the object (after `endobj` when present).
    pub end: usize,
}

impl RawObject {
    pub fn into_object(self, data: &[u8]) -> Object {
        match (self.object, self.stream) {
            (Object::Dict(dict), Some(range)) => {
                let bytes = data.get(range).unwrap_or(&[]).to_vec();
                Object::Stream(Stream { dict, data: bytes })
            }
            (object, _) => object,
        }
    }
}

pub struct Parser<'a> {
    lex: Lexer<'a>,
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Parser { lex: Lexer::new(data, pos) }
    }

    /// Current position; after a failed parse this is how far the parser got.
    pub fn pos(&self) -> usize {
        self.lex.pos()
    }

    /// Parse one direct object (which may be a reference `n g R`).
    pub fn parse_object(&mut self) -> Result<Object> {
        self.parse_value(0)
    }

    fn eof_error(&self) -> Error {
        Error::syntax(offset_u64(self.lex.pos()), "unexpected end of data")
    }

    /// `depth` is the number of containers we are already inside.
    fn parse_value(&mut self, depth: usize) -> Result<Object> {
        let start = self.lex.pos();
        let token = self.lex.next_token()?.ok_or_else(|| self.eof_error())?;
        match token {
            Token::Integer(n) => {
                if let Ok(num) = u32::try_from(n)
                    && let Some(generation) = self.lex.try_ref_suffix()
                {
                    // A file has no object above Annex C's limit, so a reference
                    // to one is a reference to nothing: null (7.3.10). It must
                    // not be a reference at all, or it could name an object a
                    // program made up for itself in that range.
                    if num > MAX_OBJECT_NUMBER {
                        return Ok(Object::Null);
                    }
                    return Ok(Object::Ref(ObjRef::new(num, generation)));
                }
                Ok(Object::Integer(n))
            }
            Token::Real(r) => Ok(Object::Real(r)),
            Token::String(s) => Ok(Object::String(s)),
            Token::Name(n) => Ok(Object::Name(n)),
            Token::ArrayStart => self.parse_array(depth),
            Token::DictStart => self.parse_dict(depth),
            Token::Keyword(b"true") => Ok(Object::Bool(true)),
            Token::Keyword(b"false") => Ok(Object::Bool(false)),
            Token::Keyword(b"null") => Ok(Object::Null),
            Token::Keyword(k) => {
                Err(Error::syntax(offset_u64(start), format!("unexpected keyword `{}`", String::from_utf8_lossy(k))))
            }
            Token::ArrayEnd | Token::DictEnd => Err(Error::syntax(offset_u64(start), "unexpected closing bracket")),
        }
    }

    /// A value inside an array or a dictionary. A keyword that has no place
    /// there (a stray token such as `"72AF..."`, which the lexer reads as a
    /// keyword because a quote is an ordinary character) reads as null and
    /// parsing goes on. The keywords that begin or end objects (`endobj`,
    /// `stream`, ...) are not skipped: they mean the container was never
    /// closed, and are an error as before.
    fn parse_member(&mut self, depth: usize) -> Result<Object> {
        let save = self.lex.pos();
        if let Some(Token::Keyword(k)) = self.lex.next_token()?
            && !matches!(k, b"true" | b"false" | b"null")
            && !is_object_boundary(k)
        {
            return Ok(Object::Null);
        }
        self.lex.set_pos(save);
        self.parse_value(depth)
    }

    fn check_depth(&self, depth: usize) -> Result<()> {
        if depth >= MAX_NESTING {
            return Err(Error::TooDeep(format!("arrays and dictionaries nested more than {MAX_NESTING} levels deep")));
        }
        Ok(())
    }

    fn parse_array(&mut self, depth: usize) -> Result<Object> {
        self.check_depth(depth)?;
        let mut items = Vec::new();
        loop {
            let save = self.lex.pos();
            match self.lex.next_token()? {
                None => return Err(self.eof_error()),
                Some(Token::ArrayEnd) => return Ok(Object::Array(items)),
                Some(_) => {
                    self.lex.set_pos(save);
                    items.push(self.parse_member(depth + 1)?);
                }
            }
        }
    }

    fn parse_dict(&mut self, depth: usize) -> Result<Object> {
        self.check_depth(depth)?;
        let mut pairs: Vec<(Name, Object)> = Vec::new();
        loop {
            let at = self.lex.pos();
            match self.lex.next_token()? {
                None => return Err(self.eof_error()),
                Some(Token::DictEnd) => return Ok(Object::Dict(Dict::from_pairs(pairs))),
                Some(Token::Name(key)) => {
                    // A key directly followed by `>>` has no value; read it as null.
                    let save = self.lex.pos();
                    if let Some(Token::DictEnd) = self.lex.next_token()? {
                        pairs.push((key, Object::Null));
                        return Ok(Object::Dict(Dict::from_pairs(pairs)));
                    }
                    self.lex.set_pos(save);
                    let value = self.parse_member(depth + 1)?;
                    pairs.push((key, value));
                }
                Some(_) => {
                    return Err(Error::syntax(offset_u64(at), "dictionary key is not a name"));
                }
            }
        }
    }

    /// `n g obj` (7.3.10).
    pub fn parse_header(&mut self) -> Result<ObjRef> {
        let start = self.lex.pos();
        let bad = |what: &str| Error::syntax(offset_u64(start), format!("expected `n g obj`: {what}"));
        let num = match self.lex.next_token()? {
            Some(Token::Integer(n)) => u32::try_from(n).map_err(|_| bad("object number out of range"))?,
            _ => return Err(bad("no object number")),
        };
        let generation = match self.lex.next_token()? {
            Some(Token::Integer(g)) => u16::try_from(g).map_err(|_| bad("generation out of range"))?,
            _ => return Err(bad("no generation number")),
        };
        match self.lex.next_token()? {
            Some(Token::Keyword(b"obj")) => Ok(ObjRef::new(num, generation)),
            _ => Err(bad("no `obj` keyword")),
        }
    }

    /// Everything after `n g obj`: the value, then `endobj` or a stream.
    pub fn parse_body(&mut self, obj_ref: ObjRef, helper: &dyn StreamHelper) -> Result<RawObject> {
        let object = self.parse_object()?;
        let save = self.lex.pos();
        // What follows the value is read leniently: garbage after the object
        // must not make a good object unreadable.
        let next = self.lex.next_token().ok().flatten();
        match next {
            Some(Token::Keyword(b"stream")) if matches!(object, Object::Dict(_)) => {
                let Object::Dict(dict) = object else {
                    return Err(Error::syntax(offset_u64(save), "stream without dictionary"));
                };
                self.read_stream(obj_ref, dict, helper)
            }
            Some(Token::Keyword(b"endobj")) => Ok(RawObject { obj_ref, object, stream: None, end: self.lex.pos() }),
            _ => {
                self.lex.set_pos(save);
                Ok(RawObject { obj_ref, object, stream: None, end: save })
            }
        }
    }

    /// A complete indirect object: header, value, `endobj` or stream.
    pub fn parse_indirect(&mut self, helper: &dyn StreamHelper) -> Result<RawObject> {
        let obj_ref = self.parse_header()?;
        self.parse_body(obj_ref, helper)
    }

    fn read_stream(&mut self, obj_ref: ObjRef, dict: Dict, helper: &dyn StreamHelper) -> Result<RawObject> {
        let data = self.lex.data();
        let after_keyword = self.lex.pos();
        let data_start = skip_stream_eol(data, after_keyword);

        // 7.3.8.2: /Length may be direct or an indirect reference.
        let length = match dict.get("Length") {
            Some(Object::Integer(n)) => Some(*n),
            Some(Object::Ref(r)) => helper.resolve_length(*r),
            _ => None,
        };
        let (data_end, resume) = stream_extent(data, data_start, length, helper)
            .ok_or_else(|| Error::syntax(offset_u64(data_start), "stream has no endstream"))?;

        self.lex.set_pos(resume);
        let save = self.lex.pos();
        let end = match self.lex.next_token().ok().flatten() {
            Some(Token::Keyword(b"endobj")) => self.lex.pos(),
            _ => {
                self.lex.set_pos(save);
                save
            }
        };
        Ok(RawObject { obj_ref, object: Object::Dict(dict), stream: Some(data_start..data_end), end })
    }
}

/// Keywords that start or end something bigger than a value: finding one inside
/// an array or dictionary means that it was left open.
fn is_object_boundary(keyword: &[u8]) -> bool {
    matches!(keyword, b"obj" | b"endobj" | b"stream" | b"endstream" | b"xref" | b"trailer" | b"startxref")
}

/// The keyword `stream` is followed by CRLF or LF (7.3.8.1). A lone CR and
/// trailing spaces before the end-of-line are tolerated.
fn skip_stream_eol(data: &[u8], pos: usize) -> usize {
    let mut q = pos;
    while matches!(data.get(q), Some(b' ' | b'\t')) {
        q += 1;
    }
    match (data.get(q), data.get(q + 1)) {
        (Some(b'\r'), Some(b'\n')) => q + 2,
        (Some(b'\n'), _) | (Some(b'\r'), _) => q + 1,
        _ => pos,
    }
}

/// If `keyword` follows `from` after optional white space, the position just
/// after it.
fn keyword_after(data: &[u8], from: usize, keyword: &[u8]) -> Option<usize> {
    let mut p = from;
    while data.get(p).is_some_and(|&b| is_whitespace(b)) {
        p += 1;
    }
    let end = p.checked_add(keyword.len())?;
    if data.get(p..end) == Some(keyword) { Some(end) } else { None }
}

/// Where the data of a stream starting at `start` ends, and where parsing
/// continues (7.3.8.2). Trust `/Length` only if `endstream` really follows
/// it; otherwise look for the first `endstream`. Returns `None` if the stream
/// cannot be delimited at all.
fn stream_extent(data: &[u8], start: usize, length: Option<i64>, helper: &dyn StreamHelper) -> Option<(usize, usize)> {
    let claimed_end =
        length.and_then(|n| usize::try_from(n).ok()).and_then(|n| start.checked_add(n)).filter(|&e| e <= data.len());
    if let Some(end) = claimed_end
        && let Some(after) = keyword_after(data, end, ENDSTREAM)
    {
        return Some((end, after));
    }
    if let Some(p) = helper.find_endstream(start) {
        // The end-of-line marker before `endstream` is not part of the data.
        let mut end = p.max(start);
        if end > start {
            match data.get(end - 1) {
                Some(b'\n') => {
                    end -= 1;
                    if end > start && data.get(end - 1) == Some(&b'\r') {
                        end -= 1;
                    }
                }
                Some(b'\r') => end -= 1,
                _ => {}
            }
        }
        return Some((end, p.saturating_add(ENDSTREAM.len())));
    }
    // No `endstream` anywhere (truncated file): trust a plausible /Length.
    claimed_end.map(|e| (e, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(input: &[u8]) -> Result<Object> {
        Parser::new(input, 0).parse_object()
    }

    struct MapHelper<'a> {
        data: &'a [u8],
        index: EndstreamIndex,
        lengths: HashMap<u32, i64>,
    }

    impl StreamHelper for MapHelper<'_> {
        fn resolve_length(&self, r: ObjRef) -> Option<i64> {
            self.lengths.get(&r.num).copied()
        }
        fn find_endstream(&self, from: usize) -> Option<usize> {
            self.index.find(self.data, from)
        }
    }

    fn parse_indirect(input: &[u8], lengths: &[(u32, i64)]) -> Result<Object> {
        let helper =
            MapHelper { data: input, index: EndstreamIndex::default(), lengths: lengths.iter().copied().collect() };
        let raw = Parser::new(input, 0).parse_indirect(&helper)?;
        Ok(raw.into_object(input))
    }

    #[test]
    fn scalars() {
        assert_eq!(parse(b"true").unwrap(), Object::Bool(true));
        assert_eq!(parse(b"false").unwrap(), Object::Bool(false));
        assert_eq!(parse(b"null").unwrap(), Object::Null);
        assert_eq!(parse(b"42").unwrap(), Object::Integer(42));
        assert_eq!(parse(b"-3.5").unwrap(), Object::Real(-3.5));
        assert_eq!(parse(b"/Foo").unwrap(), Object::from("Foo"));
        assert_eq!(parse(b"(hi)").unwrap(), Object::String(crate::object::PdfString::literal(b"hi".to_vec())));
        assert_eq!(parse(b"12 5 R").unwrap(), Object::Ref(ObjRef::new(12, 5)));
    }

    #[test]
    fn array_with_mixed_content_and_refs() {
        let obj = parse(b"[ 549 3.14 false ( Ralph ) /SomeName 7 0 R 8 1 R 9 ]").unwrap();
        let items = obj.as_array().unwrap();
        assert_eq!(items.len(), 8);
        assert_eq!(items[0], Object::Integer(549));
        assert_eq!(items[5], Object::Ref(ObjRef::new(7, 0)));
        assert_eq!(items[6], Object::Ref(ObjRef::new(8, 1)));
        assert_eq!(items[7], Object::Integer(9));
    }

    #[test]
    fn references_above_the_object_number_limit_are_null() {
        // Annex C: 8,388,607 is the most indirect objects a file may have.
        let obj = parse(b"[8388607 0 R 8388608 0 R 4294967295 0 R 9 0 R]").unwrap();
        assert_eq!(
            obj.as_array().unwrap(),
            [Object::Ref(ObjRef::new(8_388_607, 0)), Object::Null, Object::Null, Object::Ref(ObjRef::new(9, 0))]
        );
        // As a dictionary value it is the same as no value (7.3.7).
        let obj = parse(b"<< /A 4294967290 0 R /B 1 0 R >>").unwrap();
        let d = obj.as_dict().unwrap();
        assert!(!d.contains_key("A") && d.contains_key("B"));
        // A number that is not followed by `g R` is still a number.
        assert_eq!(parse(b"4294967290").unwrap(), Object::Integer(4_294_967_290));
    }

    #[test]
    fn numbers_followed_by_numbers_are_not_refs() {
        let obj = parse(b"[1 2 3 4]").unwrap();
        assert_eq!(
            obj,
            Object::Array(vec![Object::Integer(1), Object::Integer(2), Object::Integer(3), Object::Integer(4),])
        );
        // A rectangle with a number that is also a plausible object number.
        let obj = parse(b"[0 0 612 792]").unwrap();
        assert_eq!(obj.as_array().unwrap().len(), 4);
    }

    #[test]
    fn dictionary() {
        let obj = parse(
            b"<< /Type /Example /Version 0.01 /IntegerItem 12 /StringItem ( a string )
               /Subdictionary << /Item1 0.4 /Item2 true /LastItem ( not ! ) >> /Ref 4 0 R >>",
        )
        .unwrap();
        let d = obj.as_dict().unwrap();
        assert_eq!(d.get_name("Type").unwrap(), &Name::from("Example"));
        assert_eq!(d.get_int("IntegerItem"), Some(12));
        assert_eq!(d.get("Ref"), Some(&Object::Ref(ObjRef::new(4, 0))));
        let sub = d.get("Subdictionary").unwrap().as_dict().unwrap();
        assert_eq!(sub.get("Item2"), Some(&Object::Bool(true)));
    }

    #[test]
    fn dictionary_duplicates_and_nulls() {
        let obj = parse(b"<< /A 1 /A 2 /B null /C 3 >>").unwrap();
        let d = obj.as_dict().unwrap();
        assert_eq!(d.get_int("A"), Some(2));
        assert!(!d.contains_key("B"));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn huge_dictionary_is_linear_and_correct() {
        let mut input = b"<<".to_vec();
        for i in 0..50_000 {
            input.extend_from_slice(format!(" /K{i} {i}").as_bytes());
        }
        input.extend_from_slice(b" /K7 null /K8 99 >>");
        let obj = parse(&input).unwrap();
        let d = obj.as_dict().unwrap();
        assert_eq!(d.len(), 49_999);
        assert!(!d.contains_key("K7"));
        assert_eq!(d.get_int("K8"), Some(99));
        assert_eq!(d.get_int("K49999"), Some(49_999));
    }

    #[test]
    fn a_stray_keyword_in_a_value_position_reads_as_null() {
        // The quote is an ordinary character, so "72AF..." is one keyword.
        let obj = parse(b"<< /Params << /CheckSum \"72AFCDDEDF554DDA63C0C88E06F1CE18\" >> /Length 3 /B 7 >>").unwrap();
        let d = obj.as_dict().unwrap();
        let params = d.get("Params").unwrap().as_dict().unwrap();
        assert!(params.is_empty());
        assert_eq!(d.get_int("Length"), Some(3));
        assert_eq!(d.get_int("B"), Some(7));
        // In an array it is a null element, and parsing goes on.
        let obj = parse(b"[1 foo 2 <</K bar>> 3]").unwrap();
        let items = obj.as_array().unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(items[1], Object::Null);
        assert_eq!(items[2], Object::Integer(2));
        assert_eq!(items[3].as_dict().unwrap().len(), 0);
        assert_eq!(items[4], Object::Integer(3));
        // But the keywords that end objects mean the container is unclosed.
        for open in [&b"<< /A 1 /B endobj"[..], b"[1 2 endobj", b"<< /A stream", b"[ trailer", b"<< /A endstream >>", b"[1 xref"] {
            assert!(parse(open).is_err(), "{}", String::from_utf8_lossy(open));
        }
        // At the top level a keyword is still an error.
        assert!(parse(b"foo").is_err());
    }

    #[test]
    fn dictionary_key_without_value() {
        let obj = parse(b"<< /A 1 /B >>").unwrap();
        let d = obj.as_dict().unwrap();
        assert_eq!(d.get_int("A"), Some(1));
        assert!(!d.contains_key("B"));
    }

    #[test]
    fn syntax_errors() {
        assert!(parse(b"").is_err());
        assert!(parse(b"[1 2").is_err());
        assert!(parse(b"<< /A 1").is_err());
        assert!(parse(b"<< 1 2 >>").is_err());
        assert!(parse(b"]").is_err());
        assert!(parse(b">>").is_err());
        assert!(parse(b"foo").is_err());
        assert!(parse(b"endobj").is_err());
    }

    #[test]
    fn nesting_limit() {
        let ok_depth = MAX_NESTING;
        let mut ok = vec![b'['; ok_depth];
        ok.extend(std::iter::repeat_n(b']', ok_depth));
        assert!(parse(&ok).is_ok());

        let too_deep = MAX_NESTING + 1;
        let mut bad = vec![b'['; too_deep];
        bad.extend(std::iter::repeat_n(b']', too_deep));
        assert!(matches!(parse(&bad), Err(Error::TooDeep(_))));

        // 1000 deep, arrays, and unterminated: still the same error, not a stack overflow.
        let deep = vec![b'['; 1000];
        assert!(matches!(parse(&deep), Err(Error::TooDeep(_))));
        let mut dicts = Vec::new();
        for _ in 0..1000 {
            dicts.extend_from_slice(b"<</A ");
        }
        assert!(matches!(parse(&dicts), Err(Error::TooDeep(_))));
    }

    #[test]
    fn indirect_object() {
        let obj = parse_indirect(b"12 0 obj\n( Brillig )\nendobj\n", &[]).unwrap();
        assert_eq!(obj, Object::String(crate::object::PdfString::literal(b" Brillig ".to_vec())));
        let mut p = Parser::new(b"7 2 obj << /A 1 >> endobj trailing", 0);
        let helper = PlainHelper::new(b"");
        let raw = p.parse_indirect(&helper).unwrap();
        assert_eq!(raw.obj_ref, ObjRef::new(7, 2));
        assert_eq!(raw.end, "7 2 obj << /A 1 >> endobj".len());
    }

    #[test]
    fn indirect_object_without_endobj_is_tolerated() {
        let obj = parse_indirect(b"3 0 obj << /A 1 >>\n4 0 obj", &[]).unwrap();
        assert_eq!(obj.as_dict().unwrap().get_int("A"), Some(1));
    }

    #[test]
    fn bad_headers() {
        let helper = PlainHelper::new(b"");
        for input in [
            &b"x 0 obj 1 endobj"[..],
            b"1 x obj 1 endobj",
            b"1 0 foo 1 endobj",
            b"-1 0 obj 1 endobj",
            b"1 70000 obj 1 endobj",
            b"1 0",
        ] {
            assert!(Parser::new(input, 0).parse_indirect(&helper).is_err(), "{input:?}");
        }
    }

    #[test]
    fn stream_with_correct_length() {
        let input = b"5 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj\n";
        let obj = parse_indirect(input, &[]).unwrap();
        let Object::Stream(s) = obj else { panic!("not a stream") };
        assert_eq!(s.data, b"Hello");
        assert_eq!(s.dict.get_int("Length"), Some(5));
    }

    #[test]
    fn stream_with_crlf_after_keyword() {
        let input = b"5 0 obj\r\n<< /Length 5 >>\r\nstream\r\nHello\r\nendstream\r\nendobj\r\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
    }

    #[test]
    fn stream_with_lone_cr_and_trailing_spaces_after_keyword() {
        let input = b"5 0 obj << /Length 5 >> stream \r\nHello\nendstream endobj";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
        let input = b"5 0 obj << /Length 5 >> stream\rHello\nendstream endobj";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
    }

    #[test]
    fn stream_length_too_small_falls_back_to_endstream() {
        let input = b"5 0 obj\n<< /Length 2 >>\nstream\nHello World\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello World");
    }

    #[test]
    fn stream_length_too_big_falls_back_to_endstream() {
        let input = b"5 0 obj\n<< /Length 9999 >>\nstream\nHello\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
        // Length within the file but pointing past the endstream.
        let input =
            b"5 0 obj\n<< /Length 30 >>\nstream\nHello\nendstream\nendobj\n6 0 obj (xxxxxxxxxxxxxxxxxxxxx) endobj";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
    }

    #[test]
    fn stream_without_length_or_with_negative_length() {
        let input = b"5 0 obj\n<< >>\nstream\r\nHello\r\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
        let input = b"5 0 obj\n<< /Length -5 >>\nstream\nHello\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
        let input = b"5 0 obj\n<< /Length 9223372036854775807 >>\nstream\nHello\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
    }

    #[test]
    fn stream_with_indirect_length() {
        let input = b"7 0 obj\n<< /Length 8 0 R >>\nstream\nBT ET\nendstream\nendobj\n";
        // Resolvable: used (and verified).
        let Object::Stream(s) = parse_indirect(input, &[(8, 5)]).unwrap() else { panic!() };
        assert_eq!(s.data, b"BT ET");
        // Not resolvable: endstream search gives the same answer.
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"BT ET");
        // Resolvable but wrong: endstream search wins.
        let Object::Stream(s) = parse_indirect(input, &[(8, 3)]).unwrap() else { panic!() };
        assert_eq!(s.data, b"BT ET");
    }

    #[test]
    fn stream_whose_data_contains_endstream_needs_a_correct_length() {
        let input = b"5 0 obj\n<< /Length 20 >>\nstream\nab endstream cd efgh\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"ab endstream cd efgh");
    }

    #[test]
    fn empty_stream() {
        let input = b"5 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert!(s.data.is_empty());
    }

    #[test]
    fn truncated_stream_without_endstream() {
        // Good /Length, file cut right after the data: accepted.
        let input = b"5 0 obj\n<< /Length 5 >>\nstream\nHello";
        let Object::Stream(s) = parse_indirect(input, &[]).unwrap() else { panic!() };
        assert_eq!(s.data, b"Hello");
        // No usable length and no endstream: an error.
        let input = b"5 0 obj\n<< /Length 50 >>\nstream\nHello";
        assert!(parse_indirect(input, &[]).is_err());
    }

    #[test]
    fn many_broken_streams_do_not_scan_quadratically() {
        // 20,000 streams without endstream: each fallback must be O(log n).
        let mut input = Vec::new();
        let mut starts = Vec::new();
        for i in 0..20_000 {
            starts.push(input.len());
            input.extend_from_slice(format!("{} 0 obj << >> stream\nxxxxxxxxxxxxxxxxxxxxxxxx\n", i + 1).as_bytes());
        }
        let helper = PlainHelper::new(&input);
        let start = std::time::Instant::now();
        for &pos in &starts {
            let mut p = Parser::new(&input, pos);
            assert!(p.parse_indirect(&helper).is_err());
        }
        assert!(start.elapsed().as_secs() < 5);
    }
}
