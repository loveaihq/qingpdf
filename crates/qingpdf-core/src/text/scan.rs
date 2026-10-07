//! A scanner for content streams (ISO 32000-1 7.8.2, 7.2): operands and operators
//! without allocating. The general lexer builds an owned value for every name and
//! string; a page of Type 3 text has ten tokens per character, and text extraction
//! is mostly this loop.

/// Where the bytes of a string or name are: in the content stream itself, or in
/// the scanner's buffer (strings with escapes, hexadecimal strings, names with `#`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    start: u32,
    len: u32,
    in_buf: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operand {
    Num(f64),
    Str(Span),
    Name(Span),
    ArrayStart,
    ArrayEnd,
    /// A dictionary (marked-content properties): skipped over, not kept.
    Other,
}

/// What `Scanner::next` found.
pub(crate) enum Item<'a> {
    Operator(&'a [u8]),
    End,
}

pub(crate) struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
    buf: Vec<u8>,
}

/// 0: regular, 1: white space, 2: delimiter (7.2.2, Table 1).
#[allow(clippy::indexing_slicing)] // a constant table, filled with `i < 256`
const fn class_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        t[i] = match b {
            0 | 9 | 10 | 12 | 13 | 32 => 1,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' => 2,
            _ => 0,
        };
        i += 1;
    }
    t
}
static CLASS: [u8; 256] = class_table();

fn class(b: u8) -> u8 {
    CLASS.get(usize::from(b)).copied().unwrap_or(0)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

enum Tok<'a> {
    Num(f64),
    Str(Span),
    Name(Span),
    Keyword(&'a [u8]),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    End,
}

impl<'a> Scanner<'a> {
    pub fn new(data: &'a [u8]) -> Scanner<'a> {
        Scanner { data, pos: 0, buf: Vec::new() }
    }

    /// The bytes a span stands for.
    pub fn bytes(&self, s: Span) -> &[u8] {
        let src: &[u8] = if s.in_buf { &self.buf } else { self.data };
        let start = s.start as usize;
        src.get(start..start.saturating_add(s.len as usize)).unwrap_or(&[])
    }

    #[inline]
    fn skip_space(&mut self) {
        let data = self.data;
        let mut pos = self.pos;
        while let Some(&b) = data.get(pos) {
            if class(b) == 1 {
                pos += 1;
            } else if b == b'%' {
                while let Some(&c) = data.get(pos) {
                    if c == 10 || c == 13 {
                        break;
                    }
                    pos += 1;
                }
            } else {
                break;
            }
        }
        self.pos = pos;
    }

    /// Collect operands until an operator; `ops` is cleared first. Ends at the end of the
    /// data, or at a string or dictionary that is cut off (the rest cannot be read).
    pub fn next(&mut self, ops: &mut Vec<Operand>) -> Item<'a> {
        ops.clear();
        self.buf.clear();
        let data = self.data;
        loop {
            // The common tokens are read right here; the rest go through `token`.
            let mut pos = self.pos;
            while let Some(&b) = data.get(pos) {
                if class(b) == 1 {
                    pos += 1;
                } else {
                    break;
                }
            }
            self.pos = pos;
            let Some(&b) = data.get(pos) else { return Item::End };
            let operand = match b {
                b'+' | b'-' | b'.' | b'0'..=b'9' => match self.number_or_keyword() {
                    Tok::Num(v) => Operand::Num(v),
                    Tok::Keyword(k) => return Item::Operator(k),
                    _ => continue,
                },
                b'<' if data.get(pos + 1) != Some(&b'<') => {
                    self.pos += 1;
                    match self.hex_string() {
                        Tok::Str(s) => Operand::Str(s),
                        _ => return Item::End,
                    }
                }
                b'/' => {
                    self.pos += 1;
                    match self.name() {
                        Tok::Name(s) => Operand::Name(s),
                        _ => continue,
                    }
                }
                _ if class(b) == 0 => {
                    while data.get(self.pos).is_some_and(|&c| class(c) == 0) {
                        self.pos += 1;
                    }
                    return Item::Operator(data.get(pos..self.pos).unwrap_or(&[]));
                }
                _ => match self.token() {
                    Tok::Str(s) => Operand::Str(s),
                    Tok::ArrayStart => Operand::ArrayStart,
                    Tok::ArrayEnd => Operand::ArrayEnd,
                    Tok::DictStart => {
                        if !self.skip_dict() {
                            return Item::End;
                        }
                        Operand::Other
                    }
                    Tok::Keyword(k) => return Item::Operator(k),
                    Tok::End => return Item::End,
                    Tok::DictEnd | Tok::Num(_) | Tok::Name(_) => continue,
                },
            };
            if ops.len() < (1 << 20) {
                ops.push(operand);
            }
        }
    }

    /// After `<<`: skip to the matching `>>`, reading only as much as it takes to tell where
    /// strings and nested dictionaries start and end. False when the data ends first.
    fn skip_dict(&mut self) -> bool {
        let data = self.data;
        let mut pos = self.pos;
        let mut depth = 1usize;
        while let Some(&c) = data.get(pos) {
            pos += 1;
            match c {
                b'<' => {
                    if data.get(pos) == Some(&b'<') {
                        pos += 1;
                        depth += 1;
                    } else {
                        // a hexadecimal string
                        match data.get(pos..).and_then(|rest| rest.iter().position(|&x| x == b'>')) {
                            Some(n) => pos += n + 1,
                            None => break,
                        }
                    }
                }
                b'>' => {
                    if data.get(pos) == Some(&b'>') {
                        pos += 1;
                        depth -= 1;
                        if depth == 0 {
                            self.pos = pos;
                            return true;
                        }
                    }
                }
                b'(' => {
                    let mut nesting = 1usize;
                    while nesting > 0 {
                        let Some(&d) = data.get(pos) else {
                            self.pos = data.len();
                            return false;
                        };
                        pos += 1;
                        match d {
                            92 => pos += 1, // a backslash escapes the next byte
                            b'(' => nesting += 1,
                            b')' => nesting -= 1,
                            _ => {}
                        }
                    }
                }
                b'%' => {
                    while data.get(pos).is_some_and(|&x| x != 10 && x != 13) {
                        pos += 1;
                    }
                }
                _ => {}
            }
        }
        self.pos = data.len();
        false
    }

    fn token(&mut self) -> Tok<'a> {
        self.skip_space();
        let Some(&b) = self.data.get(self.pos) else { return Tok::End };
        let start = self.pos;
        match b {
            b'/' => {
                self.pos += 1;
                self.name()
            }
            b'(' => {
                self.pos += 1;
                self.literal_string()
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Tok::DictStart
                } else {
                    self.pos += 1;
                    self.hex_string()
                }
            }
            b'>' => {
                if self.data.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Tok::DictEnd
                } else {
                    self.pos += 1;
                    Tok::Keyword(self.data.get(start..self.pos).unwrap_or(&[]))
                }
            }
            b'[' => {
                self.pos += 1;
                Tok::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Tok::ArrayEnd
            }
            b'{' | b'}' | b')' => {
                self.pos += 1;
                Tok::Keyword(self.data.get(start..self.pos).unwrap_or(&[]))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.number_or_keyword(),
            _ => {
                while self.data.get(self.pos).is_some_and(|&c| class(c) == 0) {
                    self.pos += 1;
                }
                Tok::Keyword(self.data.get(start..self.pos).unwrap_or(&[]))
            }
        }
    }

    /// A number (7.3.3), read like other readers do: several signs count as one, a second
    /// period or a letter ends it. Something that starts like a number but is not one
    /// (`1e5`, `-x`) is a keyword.
    #[inline]
    fn number_or_keyword(&mut self) -> Tok<'a> {
        const POW10: [f64; 16] =
            [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15];
        let data = self.data;
        let start = self.pos;
        let mut pos = start;
        let mut negative = false;
        while let Some(&c) = data.get(pos) {
            match c {
                b'-' => negative = true,
                b'+' => {}
                _ => break,
            }
            pos += 1;
        }
        let digits_start = pos;
        let mut int: u64 = 0;
        while let Some(&c) = data.get(pos) {
            let d = c.wrapping_sub(b'0');
            if d > 9 {
                break;
            }
            if pos - digits_start < 18 {
                int = int * 10 + u64::from(d);
            }
            pos += 1;
        }
        let mut seen_digit = pos > digits_start;
        let mut value = int as f64;
        if data.get(pos) == Some(&b'.') {
            pos += 1;
            let frac_start = pos;
            let mut frac: u64 = 0;
            while let Some(&c) = data.get(pos) {
                let d = c.wrapping_sub(b'0');
                if d > 9 {
                    break;
                }
                if pos - frac_start < 15 {
                    frac = frac * 10 + u64::from(d);
                }
                pos += 1;
            }
            seen_digit |= pos > frac_start;
            let used = (pos - frac_start).min(15);
            value += frac as f64 / POW10.get(used).copied().unwrap_or(1.0);
        }
        self.pos = pos;
        // The token must end here; otherwise it is not a number.
        if !seen_digit || data.get(pos).is_some_and(|&c| class(c) == 0) {
            while data.get(self.pos).is_some_and(|&c| class(c) == 0) {
                self.pos += 1;
            }
            return Tok::Keyword(data.get(start..self.pos).unwrap_or(&[]));
        }
        Tok::Num(if negative { -value } else { value })
    }

    fn name(&mut self) -> Tok<'a> {
        let start = self.pos;
        let mut escaped = false;
        while let Some(&c) = self.data.get(self.pos) {
            if class(c) != 0 {
                break;
            }
            escaped |= c == b'#';
            self.pos += 1;
        }
        let end = self.pos;
        if !escaped {
            return Tok::Name(self.data_span(start, end));
        }
        let buf_start = self.buf.len();
        let raw = self.data.get(start..end).unwrap_or(&[]);
        let mut i = 0;
        while let Some(&c) = raw.get(i) {
            if c == b'#'
                && let (Some(h), Some(l)) = (raw.get(i + 1).and_then(|&x| hex_value(x)), raw.get(i + 2).and_then(|&x| hex_value(x)))
            {
                self.buf.push(h << 4 | l);
                i += 3;
                continue;
            }
            self.buf.push(c);
            i += 1;
        }
        Tok::Name(self.buf_span(buf_start))
    }

    fn data_span(&self, start: usize, end: usize) -> Span {
        Span { start: u32::try_from(start).unwrap_or(u32::MAX), len: u32::try_from(end - start).unwrap_or(0), in_buf: false }
    }

    fn buf_span(&self, start: usize) -> Span {
        Span {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            len: u32::try_from(self.buf.len() - start).unwrap_or(0),
            in_buf: true,
        }
    }

    /// After `(`: 7.3.4.2. Strings without escapes and line ends stay where they are.
    fn literal_string(&mut self) -> Tok<'a> {
        let start = self.pos;
        let mut depth = 1usize;
        let mut plain = true;
        let mut i = start;
        while let Some(&c) = self.data.get(i) {
            match c {
                b'\\' => {
                    plain = false;
                    i += 2;
                    continue;
                }
                b'\r' => plain = false,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if depth != 0 || i > self.data.len() {
            self.pos = self.data.len();
            return Tok::End;
        }
        let end = i;
        self.pos = end + 1;
        if plain {
            return Tok::Str(self.data_span(start, end));
        }
        let raw = self.data.get(start..end).unwrap_or(&[]);
        let buf_start = self.buf.len();
        let mut k = 0;
        while let Some(&c) = raw.get(k) {
            k += 1;
            match c {
                b'\\' => {
                    let Some(&e) = raw.get(k) else { break };
                    k += 1;
                    match e {
                        b'n' => self.buf.push(b'\n'),
                        b'r' => self.buf.push(b'\r'),
                        b't' => self.buf.push(b'\t'),
                        b'b' => self.buf.push(8),
                        b'f' => self.buf.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match raw.get(k) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        k += 1;
                                    }
                                    _ => break,
                                }
                            }
                            self.buf.push((v & 0xFF) as u8);
                        }
                        b'\r' => {
                            if raw.get(k) == Some(&b'\n') {
                                k += 1;
                            }
                        }
                        b'\n' => {}
                        other => self.buf.push(other),
                    }
                }
                b'\r' => {
                    if raw.get(k) == Some(&b'\n') {
                        k += 1;
                    }
                    self.buf.push(b'\n');
                }
                _ => self.buf.push(c),
            }
        }
        Tok::Str(self.buf_span(buf_start))
    }

    /// After `<`: 7.3.4.3.
    fn hex_string(&mut self) -> Tok<'a> {
        let buf_start = self.buf.len();
        let mut high: Option<u8> = None;
        loop {
            let Some(&c) = self.data.get(self.pos) else {
                return Tok::End;
            };
            self.pos += 1;
            if c == b'>' {
                break;
            }
            if let Some(d) = hex_value(c) {
                match high.take() {
                    Some(h) => self.buf.push(h << 4 | d),
                    None => high = Some(d),
                }
            }
        }
        if let Some(h) = high {
            self.buf.push(h << 4);
        }
        Tok::Str(self.buf_span(buf_start))
    }

    /// After an `ID` operator: 8.9.7. Skip the binary data to the end of the `EI` that ends it:
    /// `EI` between white space, with something that reads like content stream text after it.
    pub fn skip_inline_image_data(&mut self) {
        let data = self.data;
        let mut pos = self.pos;
        if data.get(pos).is_some_and(|&b| class(b) == 1) {
            pos += 1;
        }
        let mut i = pos;
        while i + 1 < data.len() {
            if data.get(i) == Some(&b'E')
                && data.get(i + 1) == Some(&b'I')
                && (i == pos || data.get(i - 1).is_some_and(|&b| class(b) == 1))
                && data.get(i + 2).is_none_or(|&b| class(b) == 1)
            {
                let tail = data.get(i + 2..(i + 2 + 16).min(data.len())).unwrap_or(&[]);
                if tail.iter().all(|&b| class(b) == 1 || (0x20..0x7F).contains(&b)) {
                    self.pos = i + 2;
                    return;
                }
            }
            i += 1;
        }
        self.pos = data.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(data: &[u8]) -> Vec<String> {
        let mut sc = Scanner::new(data);
        let mut ops = Vec::new();
        let mut out = Vec::new();
        loop {
            match sc.next(&mut ops) {
                Item::End => return out,
                Item::Operator(k) => {
                    let mut s = String::new();
                    for o in &ops {
                        match o {
                            Operand::Num(v) => s += &format!("{v} "),
                            Operand::Str(sp) => s += &format!("({}) ", String::from_utf8_lossy(sc.bytes(*sp))),
                            Operand::Name(sp) => s += &format!("/{} ", String::from_utf8_lossy(sc.bytes(*sp))),
                            Operand::ArrayStart => s += "[ ",
                            Operand::ArrayEnd => s += "] ",
                            Operand::Other => s += "<<>> ",
                        }
                    }
                    out.push(format!("{s}{}", String::from_utf8_lossy(k)));
                }
            }
        }
    }

    #[test]
    fn operands_and_operators() {
        let got = run(b"BT /F1 12 Tf 1 0 0 1 -.5 +3.25 Tm (a\\(b\\051\\n) Tj [(x) -20 <4142> ] TJ ET % comment\nQ");
        assert_eq!(
            got,
            vec!["BT", "/F1 12 Tf", "1 0 0 1 -0.5 3.25 Tm", "(a(b)\n) Tj", "[ (x) -20 (AB) ] TJ", "ET", "Q"]
        );
    }

    #[test]
    fn names_with_escapes_and_dictionaries() {
        let got = run(b"/F#31 /Span <</ActualText (a>>b) /K <</x 1>> >> BDC (t) ' EMC");
        assert_eq!(got, vec!["/F1 /Span <<>> BDC", "(t) '", "EMC"]);
    }

    #[test]
    fn odd_numbers() {
        assert_eq!(run(b"--5 1.2.3"), vec!["-5 1.2.3"]);
        assert_eq!(run(b"1e5"), vec!["1e5"]);
        assert_eq!(run(b"-.5 +7 12. w"), vec!["-0.5 7 12 w"]);
        assert_eq!(run(b"4- ."), vec!["4-", "."]);
        assert_eq!(run(b"0.1 0.25 0.001 m"), vec!["0.1 0.25 0.001 m"]);
    }

    #[test]
    fn truncated_string_ends_the_stream() {
        assert_eq!(run(b"1 0 0 1 0 0 cm (never closed"), vec!["1 0 0 1 0 0 cm".to_string()]);
    }

    #[test]
    fn inline_image_data_is_skipped() {
        let content = b"BI /W 2 /H 2 /BPC 8 /CS /G ID \x00\x01EI \xff\x02 EI\nQ";
        let mut sc = Scanner::new(content);
        let mut ops = Vec::new();
        assert!(matches!(sc.next(&mut ops), Item::Operator(b"BI")));
        assert!(matches!(sc.next(&mut ops), Item::Operator(b"ID")));
        sc.skip_inline_image_data();
        assert!(matches!(sc.next(&mut ops), Item::Operator(b"Q")));
    }
}
