//! CMaps (ISO 32000-1 9.7.5, 9.10.3): the code-to-CID maps of composite fonts
//! (predefined, embedded, `/UseCMap` chains) and the code-to-Unicode maps of
//! `/ToUnicode` (bfchar and bfrange).

use std::rc::Rc;
use std::sync::Arc;

use crate::lexer::{Lexer, Token};

use super::data::{self, Ordering};

// --- what a character becomes -----------------------------------------------------------------

/// The text one character code stands for: nothing known, one character, or
/// several (a ligature or a multi-character ToUnicode value).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Uni {
    None,
    One(char),
    Many(Rc<str>),
}

/// U+FB00 to U+FB06 as ordinary letters (decisions.md: ligatures are split).
fn ligature(c: char) -> Option<&'static str> {
    match c {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// Characters that never go into the output: controls (the output has no
/// control characters but line ends), byte order marks and non-characters.
fn dropped(c: char) -> bool {
    c.is_control() || matches!(c, '\u{FEFF}' | '\u{FFFE}' | '\u{FFFF}' | '\u{200B}'..='\u{200F}' | '\u{2028}' | '\u{2029}')
}

impl Uni {
    pub fn cp(cp: u32) -> Uni {
        match char::from_u32(data::normalize_cjk(cp)) {
            Some(c) if dropped(c) => Uni::None,
            // Spaces of other widths and the no-break space are plain spaces in the output.
            Some('\u{A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}') => Uni::One(' '),
            Some(c) => match ligature(c) {
                Some(s) => Uni::Many(Rc::from(s)),
                None => Uni::One(c),
            },
            None => Uni::None,
        }
    }

    pub fn from_str(s: &str) -> Uni {
        let mut it = s.chars();
        if let (Some(c), None) = (it.next(), it.clone().next()) {
            return Uni::cp(u32::from(c));
        }
        let mut out = String::new();
        for c in s.chars() {
            let c = char::from_u32(data::normalize_cjk(u32::from(c))).unwrap_or(c);
            if dropped(c) {
                continue;
            }
            match ligature(c) {
                Some(l) => out.push_str(l),
                None => out.push(c),
            }
        }
        match out.chars().count() {
            0 => Uni::None,
            1 => out.chars().next().map_or(Uni::None, Uni::One),
            _ => Uni::Many(Rc::from(out)),
        }
    }

    pub fn from_utf16(units: &[u16]) -> Uni {
        Uni::from_str(&String::from_utf16_lossy(units))
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Uni::None)
    }

    pub fn push_to(&self, out: &mut String) {
        match self {
            Uni::None => {}
            Uni::One(c) => out.push(*c),
            Uni::Many(s) => out.push_str(s),
        }
    }

    /// The first character, if any.
    pub fn first(&self) -> Option<char> {
        match self {
            Uni::None => None,
            Uni::One(c) => Some(*c),
            Uni::Many(s) => s.chars().next(),
        }
    }
}

// --- code to CID ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) struct CodeSpace {
    pub n: u8,
    pub lo: u32,
    pub hi: u32,
}

impl CodeSpace {
    /// 9.7.6.2: a code is in the space when each of its bytes is within the
    /// matching bytes of the low and the high bound.
    fn contains(&self, bytes: &[u8]) -> bool {
        let n = usize::from(self.n);
        if n == 0 || n > 4 || bytes.len() < n {
            return false;
        }
        (0..n).all(|i| {
            let shift = 8 * (n - 1 - i);
            let lo = (self.lo >> shift) & 0xFF;
            let hi = (self.hi >> shift) & 0xFF;
            bytes.get(i).is_some_and(|&b| (lo..=hi).contains(&u32::from(b)))
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CidRange {
    pub n: u8,
    pub lo: u32,
    pub hi: u32,
    pub cid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Identity-H/V: two-byte codes, CID = code.
    Identity,
    /// UniXXX-UCS2-H/V: two-byte codes that are Unicode values.
    Ucs2,
    /// UniXXX-UTF16-H/V: UTF-16BE codes.
    Utf16,
    /// A table of ranges (predefined legacy CMaps and embedded ones).
    Table,
}

/// How deep `/UseCMap` may go.
pub(crate) const MAX_USECMAP_DEPTH: usize = 8;

#[derive(Debug)]
pub(crate) struct CMap {
    pub name: String,
    pub kind: Kind,
    /// Code space ranges of this CMap; empty means "those of `base`".
    pub spaces: Vec<CodeSpace>,
    /// Sorted by (n, lo), not overlapping.
    pub ranges: Vec<CidRange>,
    pub wmode: u8,
    pub base: Option<Arc<CMap>>,
    /// The Adobe collection this CMap's CIDs belong to, when known.
    pub ordering: Option<Ordering>,
}

fn be(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
}

impl CMap {
    pub fn identity(wmode: u8) -> CMap {
        CMap {
            name: if wmode == 1 { "Identity-V" } else { "Identity-H" }.to_string(),
            kind: Kind::Identity,
            spaces: Vec::new(),
            ranges: Vec::new(),
            wmode,
            base: None,
            ordering: None,
        }
    }

    fn code_spaces(&self, depth: usize) -> &[CodeSpace] {
        if !self.spaces.is_empty() || depth >= MAX_USECMAP_DEPTH {
            return &self.spaces;
        }
        match &self.base {
            Some(b) => b.code_spaces(depth + 1),
            None => &self.spaces,
        }
    }

    /// Is the code the Unicode value itself (the UniXXX CMaps)?
    pub fn codes_are_unicode(&self) -> bool {
        matches!(self.kind, Kind::Ucs2 | Kind::Utf16)
    }

    /// Read the next character code of a string: (code, bytes used). Always
    /// consumes at least one byte of a non-empty string. 9.7.6.2: a code that
    /// matches no code space range uses as many bytes as the shortest range.
    pub fn next_code(&self, s: &[u8]) -> (u32, usize) {
        let Some(&first) = s.first() else { return (0, 0) };
        match self.kind {
            Kind::Identity | Kind::Ucs2 => match s.get(..2) {
                Some(two) => (be(two), 2),
                None => (u32::from(first), 1),
            },
            Kind::Utf16 => {
                let Some(two) = s.get(..2) else { return (u32::from(first), 1) };
                let unit = be(two);
                if (0xD800..0xDC00).contains(&unit)
                    && let Some(four) = s.get(..4)
                    && (0xDC00..0xE000).contains(&be(four.get(2..).unwrap_or(&[])))
                {
                    return (be(four), 4);
                }
                (unit, 2)
            }
            Kind::Table => {
                let spaces = self.code_spaces(0);
                for n in 1..=4u8 {
                    if spaces.iter().any(|sp| sp.n == n && sp.contains(s)) {
                        let used = usize::from(n);
                        return (be(s.get(..used).unwrap_or(s)), used);
                    }
                }
                let shortest = spaces
                    .iter()
                    .map(|sp| usize::from(sp.n))
                    .min()
                    .or_else(|| self.ranges.first().map(|r| usize::from(r.n)))
                    .unwrap_or(2)
                    .clamp(1, 4)
                    .min(s.len());
                (be(s.get(..shortest).unwrap_or(s)), shortest)
            }
        }
    }

    /// The CID of a code of `n` bytes (9.7.6.3). `None`: not mapped (CID 0).
    pub fn cid(&self, code: u32, n: usize) -> Option<u32> {
        self.cid_at(code, n, 0)
    }

    fn cid_at(&self, code: u32, n: usize, depth: usize) -> Option<u32> {
        match self.kind {
            Kind::Identity | Kind::Ucs2 | Kind::Utf16 => return Some(code),
            Kind::Table => {}
        }
        let n8 = u8::try_from(n).ok()?;
        let at = self.ranges.partition_point(|r| (r.n, r.lo) <= (n8, code));
        if let Some(r) = at.checked_sub(1).and_then(|i| self.ranges.get(i))
            && r.n == n8
            && code <= r.hi
        {
            return r.cid.checked_add(code - r.lo);
        }
        if depth < MAX_USECMAP_DEPTH {
            return self.base.as_ref()?.cid_at(code, n, depth + 1);
        }
        None
    }
}

/// A predefined CMap by name (Table 118): Identity, the Unicode-coded ones, and the
/// legacy ones we embed. The vertical variants (`-V`, `V`) are the horizontal one
/// with writing mode 1: they differ in the CIDs of a few punctuation marks, which
/// all lead to the same characters.
pub(crate) fn predefined(name: &str) -> Option<Arc<CMap>> {
    match name {
        "Identity-H" => return Some(Arc::new(CMap::identity(0))),
        "Identity-V" => return Some(Arc::new(CMap::identity(1))),
        _ => {}
    }
    let (h_name, wmode) = if name == "V" {
        ("H".to_string(), 1)
    } else if let Some(stem) = name.strip_suffix("-V") {
        (format!("{stem}-H"), 1)
    } else {
        (name.to_string(), 0)
    };
    if h_name.starts_with("Uni") {
        let kind = if h_name.contains("UCS2") {
            Kind::Ucs2
        } else if h_name.contains("UTF16") {
            Kind::Utf16
        } else {
            return None;
        };
        let ordering = if h_name.starts_with("UniGB") {
            Ordering::Gb1
        } else if h_name.starts_with("UniCNS") {
            Ordering::Cns1
        } else if h_name.starts_with("UniJIS") {
            Ordering::Japan1
        } else if h_name.starts_with("UniKS") {
            Ordering::Korea1
        } else {
            return None;
        };
        return Some(Arc::new(CMap {
            name: name.to_string(),
            kind,
            spaces: Vec::new(),
            ranges: Vec::new(),
            wmode,
            base: None,
            ordering: Some(ordering),
        }));
    }
    let base = data::find_predefined(&h_name)?;
    if wmode == 0 {
        return Some(base);
    }
    Some(Arc::new(CMap {
        name: name.to_string(),
        kind: Kind::Table,
        spaces: Vec::new(),
        ranges: Vec::new(),
        wmode,
        ordering: base.ordering,
        base: Some(base),
    }))
}

// --- code to Unicode (ToUnicode) ---------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) enum Dst {
    /// Code `lo + k` is the character `cp + k`.
    Cp(u32),
    Str(Rc<str>),
}

#[derive(Clone, Debug)]
pub(crate) struct UEntry {
    pub lo: u32,
    pub hi: u32,
    pub dst: Dst,
}

/// A ToUnicode CMap. Codes are compared as numbers whatever their length: many
/// producers write `<0041>` for a one-byte font.
#[derive(Debug, Default)]
pub(crate) struct ToUni {
    sorted: Vec<UEntry>,
    /// The entries in file order, kept only when some overlap: the later one wins.
    in_file_order: Option<Vec<UEntry>>,
}

/// With more entries than this and overlaps, the exact "later wins" scan is skipped
/// (it is linear per lookup) and the sorted table answers.
const MAX_OVERLAP_SCAN: usize = 4096;

impl ToUni {
    pub fn new(entries: Vec<UEntry>) -> ToUni {
        let mut sorted = entries.clone();
        sorted.sort_by_key(|e| e.lo);
        let overlap = sorted.windows(2).any(|w| matches!(w, [a, b] if b.lo <= a.hi));
        let in_file_order = (overlap && entries.len() <= MAX_OVERLAP_SCAN).then_some(entries);
        ToUni { sorted, in_file_order }
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    fn find(&self, code: u32) -> Option<&UEntry> {
        if let Some(order) = &self.in_file_order {
            return order.iter().rev().find(|e| e.lo <= code && code <= e.hi);
        }
        let at = self.sorted.partition_point(|e| e.lo <= code);
        let e = self.sorted.get(at.checked_sub(1)?)?;
        (code <= e.hi).then_some(e)
    }

    pub fn get(&self, code: u32) -> Uni {
        let Some(e) = self.find(code) else { return Uni::None };
        match &e.dst {
            Dst::Cp(cp) => Uni::cp(cp.saturating_add(code - e.lo)),
            Dst::Str(s) => Uni::from_str(s),
        }
    }
}

// --- parsing ------------------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub spaces: Vec<CodeSpace>,
    pub cid: Vec<CidRange>,
    pub bf: Vec<UEntry>,
    pub wmode: Option<u8>,
    pub use_name: Option<Vec<u8>>,
    pub ordering: Option<Vec<u8>>,
    /// The text was cut short or garbled; what came before it was kept.
    pub damaged: bool,
}

/// Most tokens and most table entries one CMap may have.
const MAX_TOKENS: usize = 6_000_000;
const MAX_ENTRIES: usize = 600_000;

#[derive(Clone)]
enum Last {
    Other,
    Int(i64),
    Name(Vec<u8>),
    Str(Vec<u8>),
}

fn string_code(bytes: &[u8]) -> Option<(u32, u8)> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    Some((be(bytes), u8::try_from(bytes.len()).ok()?))
}

/// UTF-16BE text of a ToUnicode destination; one byte alone is a byte value.
fn dst_units(bytes: &[u8]) -> Vec<u16> {
    match bytes {
        [b] => vec![u16::from(*b)],
        _ => bytes.chunks(2).map(|c| c.iter().fold(0u16, |acc, &b| (acc << 8) | u16::from(b))).collect(),
    }
}

fn add_bf(p: &mut Parsed, lo: u32, hi: u32, units: &[u16]) {
    if units.is_empty() || p.bf.len() >= MAX_ENTRIES {
        return;
    }
    let text = String::from_utf16_lossy(units);
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => p.bf.push(UEntry { lo, hi, dst: Dst::Cp(u32::from(c)) }),
        _ => {
            // Several characters: the last one counts up along the range.
            let span = hi.saturating_sub(lo).min(255);
            let (head, last) = match text.char_indices().last() {
                Some((i, c)) => (text.get(..i).unwrap_or(""), u32::from(c)),
                None => return,
            };
            for k in 0..=span {
                let c = char::from_u32(last.saturating_add(k)).unwrap_or('\u{FFFD}');
                let at = lo.saturating_add(k);
                p.bf.push(UEntry { lo: at, hi: at, dst: Dst::Str(Rc::from(format!("{head}{c}"))) });
            }
        }
    }
}

struct Cursor<'a> {
    lx: Lexer<'a>,
    tokens: usize,
    damaged: bool,
}

impl<'a> Cursor<'a> {
    /// The next token, or None at the end (a lexical error or the token budget marks damage).
    fn next(&mut self) -> Option<Token<'a>> {
        self.tokens += 1;
        if self.tokens > MAX_TOKENS {
            self.damaged = true;
            return None;
        }
        match self.lx.next_token() {
            Ok(t) => t,
            Err(_) => {
                self.damaged = true;
                None
            }
        }
    }
}

/// Read a CMap program (a stream's decoded data): code space ranges, CID ranges
/// and chars, bfchar, bfrange, `/UseCMap` (as `usecmap`), `/WMode`, `/Ordering`.
/// Garbage ends the reading; the entries read so far are kept and `damaged` is set.
pub(crate) fn parse(data: &[u8]) -> Parsed {
    let mut p = Parsed::default();
    let mut cur = Cursor { lx: Lexer::new(data, 0), tokens: 0, damaged: false };
    let mut last = [Last::Other, Last::Other];
    while let Some(tok) = cur.next() {
        match tok {
            Token::Integer(i) => last = [last[1].clone(), Last::Int(i)],
            Token::Name(n) => last = [last[1].clone(), Last::Name(n.0)],
            Token::String(s) => last = [last[1].clone(), Last::Str(s.bytes)],
            Token::Keyword(k) => {
                match k {
                    b"usecmap" => {
                        if let Last::Name(n) = &last[1] {
                            p.use_name = Some(n.clone());
                        }
                    }
                    b"def" => match (&last[0], &last[1]) {
                        (Last::Name(n), Last::Int(v)) if n == b"WMode" => p.wmode = Some(u8::from(*v == 1)),
                        (Last::Name(n), Last::Str(s)) if n == b"Ordering" => p.ordering = Some(s.clone()),
                        _ => {}
                    },
                    b"begincodespacerange" => {
                        while let Some(t) = cur.next() {
                            let Token::String(lo) = t else { break };
                            let Some(Token::String(hi)) = cur.next() else { break };
                            if let (Some((lo, n)), Some((hi, m))) = (string_code(&lo.bytes), string_code(&hi.bytes))
                                && n == m
                                && p.spaces.len() < 4096
                            {
                                p.spaces.push(CodeSpace { n, lo, hi });
                            }
                        }
                    }
                    b"begincidrange" => {
                        while let Some(t) = cur.next() {
                            let Token::String(lo) = t else { break };
                            let (Some(Token::String(hi)), Some(Token::Integer(cid))) =
                                (cur.next(), cur.next())
                            else {
                                p.damaged = true;
                                break;
                            };
                            if let (Some((lo, n)), Some((hi, m)), Ok(cid)) =
                                (string_code(&lo.bytes), string_code(&hi.bytes), u32::try_from(cid))
                                && n == m
                                && lo <= hi
                                && p.cid.len() < MAX_ENTRIES
                            {
                                p.cid.push(CidRange { n, lo, hi, cid });
                            }
                        }
                    }
                    b"begincidchar" => {
                        while let Some(t) = cur.next() {
                            let Token::String(code) = t else { break };
                            let Some(Token::Integer(cid)) = cur.next() else {
                                p.damaged = true;
                                break;
                            };
                            if let (Some((code, n)), Ok(cid)) = (string_code(&code.bytes), u32::try_from(cid))
                                && p.cid.len() < MAX_ENTRIES
                            {
                                p.cid.push(CidRange { n, lo: code, hi: code, cid });
                            }
                        }
                    }
                    b"beginbfchar" => {
                        while let Some(t) = cur.next() {
                            let Token::String(code) = t else { break };
                            match cur.next() {
                                Some(Token::String(dst)) => {
                                    if let Some((code, _)) = string_code(&code.bytes) {
                                        add_bf(&mut p, code, code, &dst_units(&dst.bytes));
                                    }
                                }
                                // `<41> /A`: a glyph name as the destination.
                                Some(Token::Name(n)) => {
                                    if let Some((code, _)) = string_code(&code.bytes) {
                                        let name = String::from_utf8_lossy(&n.0).into_owned();
                                        let cps = data::glyph_name_to_unicode(&name);
                                        let units: Vec<u16> = cps
                                            .iter()
                                            .filter_map(|&c| char::from_u32(c))
                                            .flat_map(|c| {
                                                let mut b = [0u16; 2];
                                                c.encode_utf16(&mut b).to_vec()
                                            })
                                            .collect();
                                        add_bf(&mut p, code, code, &units);
                                    }
                                }
                                _ => {
                                    p.damaged = true;
                                    break;
                                }
                            }
                        }
                    }
                    b"beginbfrange" => {
                        while let Some(t) = cur.next() {
                            let Token::String(lo) = t else { break };
                            let Some(Token::String(hi)) = cur.next() else {
                                p.damaged = true;
                                break;
                            };
                            let bounds = (string_code(&lo.bytes), string_code(&hi.bytes));
                            match cur.next() {
                                Some(Token::String(dst)) => {
                                    if let (Some((lo, _)), Some((hi, _))) = bounds
                                        && lo <= hi
                                    {
                                        add_bf(&mut p, lo, hi, &dst_units(&dst.bytes));
                                    }
                                }
                                Some(Token::ArrayStart) => {
                                    let mut k = 0u32;
                                    while let Some(item) = cur.next() {
                                        match item {
                                            Token::String(s) => {
                                                if let (Some((lo, _)), Some((hi, _))) = bounds
                                                    && lo.saturating_add(k) <= hi
                                                {
                                                    let c = lo + k;
                                                    add_bf(&mut p, c, c, &dst_units(&s.bytes));
                                                }
                                                k = k.saturating_add(1);
                                            }
                                            Token::ArrayEnd => break,
                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    p.damaged = true;
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                last = [Last::Other, Last::Other];
            }
            _ => last = [Last::Other, Last::Other],
        }
    }
    p.damaged |= cur.damaged;
    p.cid.sort_by_key(|r| (r.n, r.lo));
    p
}

impl Parsed {
    /// Build the code-to-CID map of an embedded CMap (`base`: its `/UseCMap`).
    pub fn into_cmap(self, name: String, wmode: u8, base: Option<Arc<CMap>>) -> CMap {
        let ordering = self.ordering.as_deref().and_then(Ordering::from_name).or_else(|| base.as_ref().and_then(|b| b.ordering));
        CMap { name, kind: Kind::Table, spaces: self.spaces, ranges: self.cid, wmode, base, ordering }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TO_UNICODE: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap
        /CMapName /Adobe-Identity-UCS def /CMapType 2 def
        1 begincodespacerange <0000> <FFFF> endcodespacerange
        2 beginbfchar <0003> <0020> <0005> <00660066> endbfchar
        3 beginbfrange <0010> <0012> <0041> <0020> <0021> [<4E2D> <6587>] <0030> <0030> <D840DC00> endbfrange
        endcmap end end";

    #[test]
    fn to_unicode_forms() {
        let p = parse(TO_UNICODE);
        assert!(!p.damaged);
        let t = ToUni::new(p.bf);
        assert_eq!(t.get(3), Uni::One(' '));
        assert_eq!(t.get(5), Uni::Many(Rc::from("ff")));
        assert_eq!(t.get(0x10), Uni::One('A'));
        assert_eq!(t.get(0x12), Uni::One('C'));
        assert_eq!(t.get(0x20), Uni::One('中'));
        assert_eq!(t.get(0x21), Uni::One('文'));
        assert_eq!(t.get(0x30), Uni::One('\u{20000}'));
        assert_eq!(t.get(0x99), Uni::None);
    }

    #[test]
    fn later_entry_wins_on_overlap() {
        let t = ToUni::new(vec![
            UEntry { lo: 0, hi: 0xFFFF, dst: Dst::Cp(0x4E00) },
            UEntry { lo: 0x41, hi: 0x41, dst: Dst::Cp(0x58) },
        ]);
        assert_eq!(t.get(0x41), Uni::One('X'));
        assert_eq!(t.get(0x42), Uni::One('\u{4E42}'));
    }

    #[test]
    fn embedded_cmap() {
        let p = parse(
            b"/Base usecmap begincmap /WMode 1 def 2 begincodespacerange <00> <80> <8140> <FEFE> endcodespacerange
              2 begincidrange <20> <7e> 1 <8140> <817e> 633 endcidrange 1 begincidchar <8180> 700 endcidchar endcmap",
        );
        assert_eq!(p.use_name.as_deref(), Some(&b"Base"[..]));
        assert_eq!(p.wmode, Some(1));
        let m = p.into_cmap("x".into(), 1, None);
        assert_eq!(m.next_code(&[0x41, 0x42]), (0x41, 1));
        assert_eq!(m.next_code(&[0x81, 0x41]), (0x8141, 2));
        assert_eq!(m.cid(0x41, 1), Some(0x41 - 0x20 + 1));
        assert_eq!(m.cid(0x8141, 2), Some(634));
        assert_eq!(m.cid(0x8180, 2), Some(700));
        assert_eq!(m.cid(0x9999, 2), None);
    }

    #[test]
    fn damaged_cmaps_keep_what_came_before() {
        let p = parse(b"1 begincodespacerange <00> <FF> endcodespacerange 2 begincidrange <20> <7e> 1 <80> <9f> endcidrange (unterminated");
        assert!(p.damaged);
        assert_eq!(p.spaces.len(), 1);
        assert_eq!(p.cid.len(), 1);
        assert!(!parse(b"").damaged);
    }

    #[test]
    fn predefined_names() {
        assert!(predefined("Identity-V").is_some_and(|m| m.wmode == 1));
        let v = predefined("UniGB-UCS2-V").expect("Unicode-coded CMaps exist");
        assert_eq!((v.kind, v.wmode), (Kind::Ucs2, 1));
        assert_eq!(v.next_code(&[0x4E, 0x2D, 0]), (0x4E2D, 2));
        let u16m = predefined("UniJIS-UTF16-H").expect("exists");
        assert_eq!(u16m.next_code(&[0xD8, 0x40, 0xDC, 0x00]), (0xD840_DC00, 4));
        let gbk = predefined("GBK-EUC-V").expect("exists");
        assert_eq!(gbk.wmode, 1);
        assert_eq!(gbk.next_code(&[0xB0, 0xA1]), (0xB0A1, 2));
        assert_eq!(gbk.next_code(&[0x41]), (0x41, 1));
        assert!(gbk.cid(0xB0A1, 2).is_some());
        assert!(predefined("NoSuchCMap-H").is_none());
        assert_eq!(Uni::cp(0xFB01), Uni::Many(Rc::from("fi")));
        assert_eq!(Uni::cp(0), Uni::None);
    }
}
