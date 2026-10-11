//! Lexical analysis (ISO 32000-1 7.2) and the token forms of the basic objects
//! (7.3).
//!
//! The lexer works on a byte slice and a position. It never panics: all slice
//! access goes through `get`, and every failure is an [`Error`].

use crate::error::{Error, Result};
use crate::object::{Name, PdfString};

/// White-space characters (7.2.2, Table 1): NUL, HT, LF, FF, CR, SP.
pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

/// Delimiter characters (7.2.2, Table 2).
pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// Regular characters: everything that is neither white space nor a delimiter.
pub(crate) fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Position in a file as the `u64` the error type wants.
pub(crate) fn offset_u64(pos: usize) -> u64 {
    u64::try_from(pos).unwrap_or(u64::MAX)
}

/// First occurrence of `needle` in `haystack` at or after `from`.
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    loop {
        let rest = haystack.get(i..)?;
        let at = i.checked_add(rest.iter().position(|&b| b == first)?)?;
        let end = at.checked_add(needle.len())?;
        if haystack.get(at..end) == Some(needle) {
            return Some(at);
        }
        i = at.checked_add(1)?;
    }
}

/// Last occurrence of `needle` that starts at or after `from`.
pub(crate) fn rfind_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let last_start = haystack.len().checked_sub(needle.len())?;
    let mut i = last_start;
    loop {
        if i < from {
            return None;
        }
        if haystack.get(i..i.checked_add(needle.len())?) == Some(needle) {
            return Some(i);
        }
        i = i.checked_sub(1)?;
    }
}

/// A lexical token. Keywords borrow from the input.
#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Integer(i64),
    Real(f64),
    String(PdfString),
    Name(Name),
    /// A run of regular characters that is not a number (`obj`, `true`, `R`, ...)
    /// or a lone stray delimiter (`)`, `{`, `}`, a single `>`).
    Keyword(&'a [u8]),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
}

pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Lexer { data, pos: pos.min(data.len()) }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.data.get(self.pos.checked_add(ahead)?).copied()
    }

    fn slice(&self, from: usize, to: usize) -> &'a [u8] {
        self.data.get(from..to).unwrap_or(&[])
    }

    /// Skip white space and comments (7.2.2, 7.2.3). A comment runs to the end
    /// of the line and counts as one white-space character.
    pub fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Next token, or `None` at the end of the data.
    pub fn next_token(&mut self) -> Result<Option<Token<'a>>> {
        self.skip_whitespace();
        let start = self.pos;
        let Some(b) = self.peek() else {
            return Ok(None);
        };
        let token = match b {
            b'/' => {
                self.pos += 1;
                Token::Name(self.read_name())
            }
            b'(' => {
                self.pos += 1;
                Token::String(self.read_literal_string(start)?)
            }
            b'<' => {
                if self.peek_at(1) == Some(b'<') {
                    self.pos += 2;
                    Token::DictStart
                } else {
                    self.pos += 1;
                    Token::String(self.read_hex_string(start)?)
                }
            }
            b'>' => {
                if self.peek_at(1) == Some(b'>') {
                    self.pos += 2;
                    Token::DictEnd
                } else {
                    self.pos += 1;
                    Token::Keyword(self.slice(start, self.pos))
                }
            }
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'{' | b'}' | b')' => {
                self.pos += 1;
                Token::Keyword(self.slice(start, self.pos))
            }
            _ => {
                while self.peek().is_some_and(is_regular) {
                    self.pos += 1;
                }
                let word = self.slice(start, self.pos);
                parse_number(word).unwrap_or(Token::Keyword(word))
            }
        };
        Ok(Some(token))
    }

    /// Name after the solidus (7.3.5). `#xx` is one byte; a `#` not followed
    /// by two hex digits is kept literally (tolerance for pre-1.2 files).
    fn read_name(&mut self) -> Name {
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hi = self.peek().and_then(hex_value);
                let lo = self.peek_at(1).and_then(hex_value);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    self.pos += 2;
                    out.push(hi << 4 | lo);
                    continue;
                }
            }
            out.push(b);
        }
        Name(out)
    }

    /// Literal string (7.3.4.2); the opening parenthesis is already consumed.
    fn read_literal_string(&mut self, start: usize) -> Result<PdfString> {
        let mut out = Vec::new();
        let mut depth: usize = 1;
        loop {
            let Some(b) = self.peek() else {
                return Err(Error::syntax(offset_u64(start), "unterminated literal string"));
            };
            self.pos += 1;
            match b {
                b'(' => {
                    depth = depth.saturating_add(1);
                    out.push(b);
                }
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(c) = self.peek() else {
                        return Err(Error::syntax(offset_u64(start), "unterminated literal string"));
                    };
                    self.pos += 1;
                    match c {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            // One to three octal digits; high-order overflow is ignored.
                            let mut value = u32::from(c - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        value = value * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((value & 0xFF) as u8);
                        }
                        // A backslash at the end of a line continues the string:
                        // both the backslash and the end-of-line marker vanish.
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        // `\(`, `\)`, `\\` and any other character stand for
                        // themselves: the backslash is ignored.
                        other => out.push(other),
                    }
                }
                // An unescaped end-of-line marker is one LF, whichever form it took.
                b'\r' => {
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        Ok(PdfString::literal(out))
    }

    /// Hexadecimal string (7.3.4.3); the `<` is already consumed. White space
    /// is ignored, an odd final digit counts as if followed by 0. Other
    /// characters are ignored too (tolerance).
    fn read_hex_string(&mut self, start: usize) -> Result<PdfString> {
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        loop {
            let Some(b) = self.peek() else {
                return Err(Error::syntax(offset_u64(start), "unterminated hexadecimal string"));
            };
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(d) = hex_value(b) {
                match high.take() {
                    Some(h) => out.push(h << 4 | d),
                    None => high = Some(d),
                }
            }
        }
        if let Some(h) = high {
            out.push(h << 4);
        }
        Ok(PdfString::hex(out))
    }

    /// After an integer token: if what follows is `<generation> R`, consume it
    /// and return the generation (7.3.10). Otherwise leave the position
    /// untouched.
    pub fn try_ref_suffix(&mut self) -> Option<u16> {
        let save = self.pos;
        let result = self.ref_suffix_inner();
        if result.is_none() {
            self.pos = save;
        }
        result
    }

    fn ref_suffix_inner(&mut self) -> Option<u16> {
        self.skip_whitespace();
        let digits_start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits = self.slice(digits_start, self.pos);
        if digits.is_empty() || digits.len() > 5 {
            return None;
        }
        let generation = std::str::from_utf8(digits).ok()?.parse::<u32>().ok()?;
        let generation = u16::try_from(generation).ok()?;
        let after_digits = self.pos;
        self.skip_whitespace();
        if self.pos == after_digits || self.peek() != Some(b'R') {
            return None;
        }
        self.pos += 1;
        if self.peek().is_some_and(is_regular) {
            return None;
        }
        Some(generation)
    }
}

/// A numeric token (7.3.3): optional sign, digits, optionally a decimal point
/// with leading and/or trailing digits (`+.5`, `-.002`, `4.`). Integers that do
/// not fit an `i64` become reals.
fn parse_number(word: &[u8]) -> Option<Token<'static>> {
    let body = match word.first()? {
        b'+' | b'-' => word.get(1..)?,
        _ => word,
    };
    let mut digits = 0usize;
    let mut dots = 0usize;
    for &b in body {
        match b {
            b'0'..=b'9' => digits += 1,
            b'.' => dots += 1,
            _ => return None,
        }
    }
    if digits == 0 || dots > 1 {
        return None;
    }
    let text = std::str::from_utf8(word).ok()?;
    if dots == 0
        && let Ok(i) = text.parse::<i64>()
    {
        return Some(Token::Integer(i));
    }
    let value = text.parse::<f64>().ok()?;
    if value.is_finite() { Some(Token::Real(value)) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_tokens(input: &[u8]) -> Vec<Token<'_>> {
        let mut lexer = Lexer::new(input, 0);
        let mut out = Vec::new();
        while let Some(t) = lexer.next_token().unwrap() {
            out.push(t);
        }
        out
    }

    fn one(input: &[u8]) -> Token<'_> {
        let mut tokens = all_tokens(input);
        assert_eq!(tokens.len(), 1, "expected exactly one token in {input:?}");
        tokens.remove(0)
    }

    #[test]
    fn numbers() {
        assert_eq!(one(b"123"), Token::Integer(123));
        assert_eq!(one(b"+17"), Token::Integer(17));
        assert_eq!(one(b"-98"), Token::Integer(-98));
        assert_eq!(one(b"0"), Token::Integer(0));
        assert_eq!(one(b"34.5"), Token::Real(34.5));
        assert_eq!(one(b"-3.62"), Token::Real(-3.62));
        assert_eq!(one(b"+123.6"), Token::Real(123.6));
        assert_eq!(one(b"4."), Token::Real(4.0));
        assert_eq!(one(b"-.002"), Token::Real(-0.002));
        assert_eq!(one(b"+.5"), Token::Real(0.5));
        assert_eq!(one(b"0.0"), Token::Real(0.0));
        // Too big for i64: becomes a real instead of failing.
        assert!(matches!(one(b"99999999999999999999"), Token::Real(_)));
    }

    #[test]
    fn non_numbers_are_keywords() {
        assert_eq!(one(b"-"), Token::Keyword(b"-"));
        assert_eq!(one(b"."), Token::Keyword(b"."));
        assert_eq!(one(b"1.2.3"), Token::Keyword(b"1.2.3"));
        assert_eq!(one(b"12abc"), Token::Keyword(b"12abc"));
        assert_eq!(one(b"obj"), Token::Keyword(b"obj"));
    }

    #[test]
    fn whitespace_and_comments() {
        let tokens = all_tokens(b"\0 \t\x0c\r\n abc% comment ( /% ) blah\n123");
        assert_eq!(tokens, vec![Token::Keyword(b"abc"), Token::Integer(123)]);
        // A comment ends at CR as well as LF.
        let tokens = all_tokens(b"1 % x\r2");
        assert_eq!(tokens, vec![Token::Integer(1), Token::Integer(2)]);
    }

    #[test]
    fn literal_string_basics() {
        assert_eq!(one(b"()"), Token::String(PdfString::literal(b"".to_vec())));
        assert_eq!(
            one(b"( It has zero ( 0 ) length . )"),
            Token::String(PdfString::literal(b" It has zero ( 0 ) length . ".to_vec()))
        );
        // Nested parentheses to depth 3.
        assert_eq!(one(b"(a(b(c)d)e)"), Token::String(PdfString::literal(b"a(b(c)d)e".to_vec())));
    }

    #[test]
    fn literal_string_escapes() {
        let t = one(b"(\\n\\r\\t\\b\\f\\(\\)\\\\)");
        assert_eq!(t, Token::String(PdfString::literal(b"\n\r\t\x08\x0c()\\".to_vec())));
        // Octal: one, two and three digits; \0053 is \005 followed by '3'.
        assert_eq!(one(b"(\\53)"), Token::String(PdfString::literal(b"+".to_vec())));
        assert_eq!(one(b"(\\053)"), Token::String(PdfString::literal(b"+".to_vec())));
        assert_eq!(one(b"(\\0053)"), Token::String(PdfString::literal(vec![5, b'3'])));
        // High-order overflow is ignored: \400 == 0x100 -> 0x00.
        assert_eq!(one(b"(\\400)"), Token::String(PdfString::literal(vec![0])));
        assert_eq!(
            one(b"(\\245two\\307)"),
            Token::String(PdfString::literal([&[0xA5u8][..], b"two", &[0xC7u8][..]].concat()))
        );
        // Unknown escape: the backslash is dropped.
        assert_eq!(one(b"(\\q)"), Token::String(PdfString::literal(b"q".to_vec())));
    }

    #[test]
    fn literal_string_line_handling() {
        // Backslash-EOL continues the line, for LF, CR and CRLF.
        for eol in [&b"\n"[..], b"\r", b"\r\n"] {
            let mut input = b"(These \\".to_vec();
            input.extend_from_slice(eol);
            input.extend_from_slice(b"two)");
            assert_eq!(one(&input), Token::String(PdfString::literal(b"These two".to_vec())));
        }
        // A bare EOL inside the string is one LF whichever form it has.
        for eol in [&b"\n"[..], b"\r", b"\r\n"] {
            let mut input = b"(a".to_vec();
            input.extend_from_slice(eol);
            input.extend_from_slice(b"b)");
            assert_eq!(one(&input), Token::String(PdfString::literal(b"a\nb".to_vec())));
        }
    }

    #[test]
    fn unterminated_strings_are_errors() {
        let mut l = Lexer::new(b"(abc", 0);
        assert!(l.next_token().is_err());
        let mut l = Lexer::new(b"<4142", 0);
        assert!(l.next_token().is_err());
        let mut l = Lexer::new(b"(abc\\", 0);
        assert!(l.next_token().is_err());
    }

    #[test]
    fn hex_strings() {
        assert_eq!(one(b"<4E6F76>"), Token::String(PdfString::hex(b"Nov".to_vec())));
        // White space ignored.
        assert_eq!(one(b"< 90 1F\nA3 >"), Token::String(PdfString::hex(vec![0x90, 0x1F, 0xA3])));
        // Odd digit count: trailing 0.
        assert_eq!(one(b"<901FA>"), Token::String(PdfString::hex(vec![0x90, 0x1F, 0xA0])));
        assert_eq!(one(b"<>"), Token::String(PdfString::hex(vec![])));
        assert_eq!(one(b"<abCD>"), Token::String(PdfString::hex(vec![0xAB, 0xCD])));
    }

    #[test]
    fn names() {
        assert_eq!(one(b"/Name1"), Token::Name(Name::from("Name1")));
        assert_eq!(one(b"/"), Token::Name(Name::from("")));
        assert_eq!(one(b"/lime#20Green"), Token::Name(Name::from("lime Green")));
        assert_eq!(one(b"/paired#28#29parentheses"), Token::Name(Name::from("paired()parentheses")));
        assert_eq!(one(b"/The_Key_of_F#23_Minor"), Token::Name(Name::from("The_Key_of_F#_Minor")));
        assert_eq!(one(b"/A#42"), Token::Name(Name::from("AB")));
        // A `#` without two hex digits stays as it is.
        assert_eq!(one(b"/A#4"), Token::Name(Name::from("A#4")));
        assert_eq!(one(b"/A#zz"), Token::Name(Name::from("A#zz")));
        // Names end at delimiters and white space.
        let tokens = all_tokens(b"/A/B[/C]");
        assert_eq!(
            tokens,
            vec![
                Token::Name(Name::from("A")),
                Token::Name(Name::from("B")),
                Token::ArrayStart,
                Token::Name(Name::from("C")),
                Token::ArrayEnd,
            ]
        );
    }

    #[test]
    fn delimiters_and_stray_characters() {
        let tokens = all_tokens(b"<</A 1>>");
        assert_eq!(tokens, vec![Token::DictStart, Token::Name(Name::from("A")), Token::Integer(1), Token::DictEnd,]);
        assert_eq!(all_tokens(b") } {"), vec![Token::Keyword(b")"), Token::Keyword(b"}"), Token::Keyword(b"{"),]);
        assert_eq!(all_tokens(b">"), vec![Token::Keyword(b">")]);
    }

    #[test]
    fn ref_suffix() {
        let mut l = Lexer::new(b"12 0 R rest", 0);
        assert_eq!(l.next_token().unwrap(), Some(Token::Integer(12)));
        assert_eq!(l.try_ref_suffix(), Some(0));
        assert_eq!(l.next_token().unwrap(), Some(Token::Keyword(b"rest")));

        // Not a reference: position must be untouched.
        let mut l = Lexer::new(b"12 0 obj", 0);
        assert_eq!(l.next_token().unwrap(), Some(Token::Integer(12)));
        assert_eq!(l.try_ref_suffix(), None);
        assert_eq!(l.next_token().unwrap(), Some(Token::Integer(0)));

        let mut l = Lexer::new(b"12 0 Rx", 0);
        l.next_token().unwrap();
        assert_eq!(l.try_ref_suffix(), None);

        // Generation too large for u16.
        let mut l = Lexer::new(b"12 70000 R", 0);
        l.next_token().unwrap();
        assert_eq!(l.try_ref_suffix(), None);

        // Reference right before a delimiter.
        let mut l = Lexer::new(b"1 0 R]", 0);
        l.next_token().unwrap();
        assert_eq!(l.try_ref_suffix(), Some(0));
    }

    #[test]
    fn byte_search_helpers() {
        assert_eq!(find_bytes(b"abcabc", b"bc", 0), Some(1));
        assert_eq!(find_bytes(b"abcabc", b"bc", 2), Some(4));
        assert_eq!(find_bytes(b"abcabc", b"zz", 0), None);
        assert_eq!(find_bytes(b"abc", b"", 0), None);
        assert_eq!(find_bytes(b"abc", b"abc", 10), None);
        assert_eq!(rfind_bytes(b"abcabc", b"bc", 0), Some(4));
        assert_eq!(rfind_bytes(b"abcabc", b"bc", 5), None);
        assert_eq!(rfind_bytes(b"ab", b"abc", 0), None);
    }

    #[test]
    fn lexer_on_arbitrary_bytes_never_panics() {
        // Every byte value, alone and in pairs.
        for a in 0..=255u8 {
            let single = [a];
            let mut l = Lexer::new(&single, 0);
            let _ = l.next_token();
            for b in 0..=255u8 {
                let input = [a, b, a];
                let mut l = Lexer::new(&input, 0);
                while let Ok(Some(_)) = l.next_token() {}
            }
        }
    }
}
