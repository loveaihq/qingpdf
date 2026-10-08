//! Type 1 outlines (Adobe Type 1 Font Format): the eexec-encrypted private part of a font program
//! (`/FontFile`, also with PFB segment headers), its Subrs and CharStrings, and the charstring
//! interpreter with flex, hint replacement and `seac`.
//!
//! Limits: a charstring runs at most [`MAX_TICKS`] operators and operands (and bytes of the glyph and of every
//! subroutine called, however short its run), calls subroutines [`MAX_SUBR_DEPTH`] deep, keeps [`MAX_STACK`] numbers; the font keeps at most [`MAX_ENTRIES`]
//! glyphs and as many subroutines. Whatever cannot be read makes the font an error.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::text::data;
use crate::text::fontprog::{self, Lexer, Res, Tok};

use super::outline::Builder;

const MAX_TICKS: usize = 100_000;
const MAX_SUBR_DEPTH: usize = 10;
const MAX_STACK: usize = 48;
const MAX_ENTRIES: usize = 65_535;
/// Tokens read from the private part before the font is called broken.
const MAX_TOKENS: usize = 2_000_000;
/// The PostScript stack of `callothersubr`/`pop`, and flex points.
const MAX_PS: usize = 24;

pub(crate) struct T1Font {
    private: Vec<u8>,
    /// Bytes of random data in front of each charstring (`/lenIV`); negative: not encrypted.
    len_iv: i32,
    subrs: Vec<Option<(usize, usize)>>,
    /// The decrypted subroutines, made when first called (at most as many bytes as the private part has).
    subr_cache: Vec<OnceLock<Option<Box<[u8]>>>>,
    glyphs: Vec<(usize, usize)>,
    by_name: HashMap<String, usize>,
    matrix: [f64; 6],
    /// The font's own encoding: names by code, `None` for StandardEncoding.
    encoding: Option<Vec<Option<Vec<u8>>>>,
    by_unicode: OnceLock<HashMap<u32, usize>>,
}

fn decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    let mut r = key;
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = u16::from(c).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
    }
    out.drain(..skip.min(out.len()));
    out
}

/// The cleartext and the encrypted part of a program, with the PFB headers (0x80 and a segment
/// type) taken off, the hexadecimal form of the encrypted part turned into bytes.
fn split(program: &[u8]) -> Res<(Vec<u8>, Vec<u8>)> {
    let (clear, binary): (Vec<u8>, Vec<u8>) = if program.first() == Some(&0x80) {
        let (mut clear, mut binary) = (Vec::new(), Vec::new());
        let mut pos = 0usize;
        while program.get(pos) == Some(&0x80) {
            let kind = *program.get(pos + 1).ok_or("truncated PFB segment")?;
            if kind == 3 {
                break;
            }
            let len = usize::try_from(fontprog::u32_at_le(program, pos + 2).ok_or("truncated PFB segment")?).map_err(|_| "bad PFB segment")?;
            let body = program.get(pos + 6..pos.checked_add(6).and_then(|p| p.checked_add(len)).ok_or("bad PFB segment")?).ok_or("truncated PFB segment")?;
            match kind {
                1 => clear.extend_from_slice(body),
                2 => binary.extend_from_slice(body),
                _ => return Err("bad PFB segment"),
            }
            pos += 6 + len;
        }
        (clear, binary)
    } else {
        let at = fontprog::find(program, b"eexec").ok_or("no eexec section")? + 5;
        let mut start = at;
        while program.get(start).is_some_and(|&b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
            start += 1;
        }
        (program.get(..at).unwrap_or_default().to_vec(), program.get(start..).unwrap_or_default().to_vec())
    };
    // Four hexadecimal digits at the start: the whole part is hexadecimal.
    let hex = binary.get(..4).is_some_and(|f| f.iter().all(u8::is_ascii_hexdigit));
    if hex {
        let mut out = Vec::with_capacity(binary.len() / 2);
        let mut high: Option<u8> = None;
        for &b in &binary {
            let v = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                b' ' | b'\t' | b'\r' | b'\n' => continue,
                _ => break,
            };
            match high.take() {
                Some(h) => out.push(h << 4 | v),
                None => high = Some(v),
            }
        }
        return Ok((clear, out));
    }
    Ok((clear, binary))
}

fn word_int(t: Option<Tok<'_>>) -> Option<i64> {
    match t? {
        Tok::Word(w) => std::str::from_utf8(w).ok()?.parse::<i64>().ok(),
        Tok::Name(_) => None,
    }
}

impl T1Font {
    pub fn parse(program: &[u8]) -> Res<T1Font> {
        let (clear, binary) = split(program)?;
        if binary.len() < 8 {
            return Err("the encrypted part of the font is missing");
        }
        let private = decrypt(&binary, 55665, 4);
        // FontMatrix [a b c d e f]
        let mut matrix = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
        if let Some(at) = fontprog::find(&clear, b"/FontMatrix") {
            let mut lx = Lexer { s: clear.get(at + 11..).unwrap_or_default(), pos: 0 };
            let mut nums = Vec::new();
            for _ in 0..8 {
                match lx.next_token() {
                    Some(Tok::Word(w)) if w == b"[" || w == b"{" => {}
                    Some(Tok::Word(w)) => match std::str::from_utf8(w).ok().and_then(|t| t.parse::<f64>().ok()) {
                        Some(v) if v.is_finite() => nums.push(v),
                        _ => break,
                    },
                    _ => break,
                }
                if nums.len() == 6 {
                    break;
                }
            }
            if let Ok(m) = <[f64; 6]>::try_from(nums.as_slice())
                && (m[0] * m[3] - m[1] * m[2]).abs() > 1e-12
            {
                matrix = m;
            }
        }
        let encoding = fontprog::type1_encoding(program).unwrap_or(None);

        let mut font = T1Font {
            private,
            len_iv: 4,
            subrs: Vec::new(),
            subr_cache: Vec::new(),
            glyphs: Vec::new(),
            by_name: HashMap::new(),
            matrix,
            encoding,
            by_unicode: OnceLock::new(),
        };
        font.read_private()?;
        if font.glyphs.is_empty() {
            return Err("the font has no CharStrings");
        }
        Ok(font)
    }

    /// Find `/lenIV`, the Subrs and the CharStrings in the decrypted part.
    fn read_private(&mut self) -> Res<()> {
        let private = std::mem::take(&mut self.private);
        let mut lx = Lexer { s: &private, pos: 0 };
        let (mut in_subrs, mut in_chars) = (false, false);
        let mut subrs: Vec<Option<(usize, usize)>> = Vec::new();
        let mut glyphs: Vec<(usize, usize)> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut len_iv = 4i32;
        let mut previous_name: Option<Vec<u8>> = None;
        for _ in 0..MAX_TOKENS {
            let Some(tok) = lx.next_token() else { break };
            match tok {
                Tok::Name(n) if n == b"lenIV" => {
                    len_iv = word_int(lx.next_token()).and_then(|v| i32::try_from(v).ok()).unwrap_or(4);
                }
                Tok::Name(n) if n == b"Subrs" => in_subrs = true,
                Tok::Name(n) if n == b"CharStrings" => {
                    in_chars = true;
                    in_subrs = false;
                }
                Tok::Word(w) if w == b"dup" && in_subrs => {
                    let (Some(index), Some(len)) = (word_int(lx.next_token()), word_int(lx.next_token())) else { continue };
                    let _rd = lx.next_token();
                    let (Ok(index), Ok(len)) = (usize::try_from(index), usize::try_from(len)) else { continue };
                    let start = lx.pos + 1;
                    let end = start.checked_add(len).filter(|&e| e <= private.len()).ok_or("a Subr runs past the end of the font")?;
                    if index >= MAX_ENTRIES {
                        return Err("a Subr number is too large");
                    }
                    if subrs.len() <= index {
                        subrs.resize(index + 1, None);
                    }
                    if let Some(slot) = subrs.get_mut(index) {
                        *slot = Some((start, len));
                    }
                    lx.pos = end;
                }
                Tok::Name(n) if in_chars => previous_name = Some(n.to_vec()),
                Tok::Word(w) if in_chars && previous_name.is_some() => {
                    // `/name <length> RD <bytes> ND`: this word is the length.
                    let name = previous_name.take().unwrap_or_default();
                    let Some(len) = std::str::from_utf8(w).ok().and_then(|t| t.parse::<usize>().ok()) else { continue };
                    let _rd = lx.next_token();
                    let start = lx.pos + 1;
                    let end = start.checked_add(len).filter(|&e| e <= private.len()).ok_or("a CharString runs past the end of the font")?;
                    if glyphs.len() >= MAX_ENTRIES {
                        return Err("too many glyphs");
                    }
                    glyphs.push((start, len));
                    names.push(String::from_utf8_lossy(&name).into_owned());
                    lx.pos = end;
                }
                Tok::Word(w) if in_chars && w == b"end" && !glyphs.is_empty() => break,
                _ => previous_name = None,
            }
        }
        self.private = private;
        self.len_iv = len_iv;
        self.subr_cache = subrs.iter().map(|_| OnceLock::new()).collect();
        self.subrs = subrs;
        self.glyphs = glyphs;
        for (i, n) in names.into_iter().enumerate() {
            self.by_name.entry(n).or_insert(i);
        }
        Ok(())
    }

    pub fn builder(&self) -> Builder {
        Builder::new(self.matrix)
    }

    /// Glyph numbers are the position in the CharStrings dictionary plus one (the first entry of a Type 1
    /// font is not necessarily `.notdef`, so 0 means no glyph).
    pub fn gid_of_name(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).and_then(|&g| u32::try_from(g).ok()).map(|g| g + 1)
    }

    pub fn gid_of_unicode(&self, cp: u32) -> Option<u32> {
        let map = self.by_unicode.get_or_init(|| {
            let mut map: HashMap<u32, usize> = HashMap::new();
            for (name, &gid) in &self.by_name {
                if name == ".notdef" {
                    continue;
                }
                if let [one] = data::glyph_name_to_unicode(name).as_slice() {
                    let slot = map.entry(*one).or_insert(gid);
                    *slot = (*slot).min(gid);
                }
            }
            map
        });
        map.get(&cp).and_then(|&g| u32::try_from(g).ok()).map(|g| g + 1)
    }

    /// The glyph the font's own encoding gives a code.
    pub fn gid_of_code(&self, code: u8) -> Option<u32> {
        match &self.encoding {
            None => self.gid_of_name(fontprog::standard_encoding_name(code)?),
            Some(names) => {
                let name = names.get(usize::from(code))?.as_ref()?;
                self.gid_of_name(&String::from_utf8_lossy(name))
            }
        }
    }

    fn charstring(&self, range: (usize, usize)) -> Option<Vec<u8>> {
        let raw = self.private.get(range.0..range.0.checked_add(range.1)?)?;
        Some(match usize::try_from(self.len_iv) {
            Ok(skip) => decrypt(raw, 4330, skip),
            Err(_) => raw.to_vec(),
        })
    }

    /// Subroutine `index`, decrypted once per font however often it is called.
    fn subr(&self, index: usize) -> Option<&[u8]> {
        let range = self.subrs.get(index).copied().flatten()?;
        let cell = self.subr_cache.get(index)?;
        cell.get_or_init(|| self.charstring(range).map(Vec::into_boxed_slice)).as_deref()
    }

    pub fn outline(&self, gid: u32, out: &mut Builder) -> Res<()> {
        let mut run = Run { font: self, stack: Vec::new(), ps: Vec::new(), x: 0.0, y: 0.0, sbx: 0.0, ox: 0.0, oy: 0.0, flex: None, ticks: 0, seac: false };
        let result = usize::try_from(gid).ok().and_then(|g| g.checked_sub(1)).ok_or("glyph number out of range").and_then(|g| run.glyph(g, out));
        out.charge(run.ticks);
        result?;
        out.close()
    }
}

struct Run<'a> {
    font: &'a T1Font,
    stack: Vec<f64>,
    /// The PostScript stack `callothersubr` leaves for `pop`.
    ps: Vec<f64>,
    x: f64,
    y: f64,
    sbx: f64,
    ox: f64,
    oy: f64,
    /// Points of a flex in progress.
    flex: Option<Vec<(f64, f64)>>,
    ticks: usize,
    seac: bool,
}

enum Flow {
    Go,
    Return,
    End,
}

impl Run<'_> {
    fn glyph(&mut self, gid: usize, out: &mut Builder) -> Res<()> {
        let range = *self.font.glyphs.get(gid).ok_or("glyph number out of range")?;
        let cs = self.font.charstring(range).ok_or("a CharString cannot be read")?;
        self.ticks = self.ticks.saturating_add(cs.len());
        self.stack.clear();
        self.ps.clear();
        self.flex = None;
        self.x = 0.0;
        self.y = 0.0;
        self.exec(&cs, 0, out)?;
        Ok(())
    }

    fn push(&mut self, v: f64) -> Res<()> {
        if self.stack.len() >= MAX_STACK {
            return Err("the charstring stack overflows");
        }
        self.stack.push(v);
        Ok(())
    }

    fn pop(&mut self) -> f64 {
        self.stack.pop().unwrap_or(0.0)
    }

    fn arg(&self, from_end: usize) -> f64 {
        self.stack.len().checked_sub(from_end).and_then(|i| self.stack.get(i)).copied().unwrap_or(0.0)
    }

    fn move_by(&mut self, dx: f64, dy: f64, out: &mut Builder) -> Res<()> {
        self.x += dx;
        self.y += dy;
        if self.flex.is_none() {
            out.move_to(self.ox + self.x, self.oy + self.y)?;
        }
        Ok(())
    }

    fn line_by(&mut self, dx: f64, dy: f64, out: &mut Builder) -> Res<()> {
        self.x += dx;
        self.y += dy;
        out.line_to(self.ox + self.x, self.oy + self.y)
    }

    fn curve_by(&mut self, d: [f64; 6], out: &mut Builder) -> Res<()> {
        let (x1, y1) = (self.x + d[0], self.y + d[1]);
        let (x2, y2) = (x1 + d[2], y1 + d[3]);
        let (x3, y3) = (x2 + d[4], y2 + d[5]);
        self.x = x3;
        self.y = y3;
        let (ox, oy) = (self.ox, self.oy);
        out.cubic_to(ox + x1, oy + y1, ox + x2, oy + y2, ox + x3, oy + y3)
    }

    fn exec(&mut self, cs: &[u8], depth: usize, out: &mut Builder) -> Res<Flow> {
        let mut pos = 0usize;
        while let Some(&b0) = cs.get(pos) {
            pos += 1;
            self.ticks += 1;
            if self.ticks > MAX_TICKS {
                return Err("the charstring runs too long");
            }
            match b0 {
                32..=246 => self.push(f64::from(b0) - 139.0)?,
                247..=250 => {
                    let b1 = *cs.get(pos).ok_or("truncated charstring")?;
                    pos += 1;
                    self.push((f64::from(b0) - 247.0) * 256.0 + f64::from(b1) + 108.0)?;
                }
                251..=254 => {
                    let b1 = *cs.get(pos).ok_or("truncated charstring")?;
                    pos += 1;
                    self.push(-(f64::from(b0) - 251.0) * 256.0 - f64::from(b1) - 108.0)?;
                }
                255 => {
                    let v = fontprog::u32_at(cs, pos).ok_or("truncated charstring")?;
                    pos += 4;
                    self.push(f64::from(v as i32))?;
                }
                1 | 3 => self.stack.clear(),
                4 => {
                    let dy = self.arg(1);
                    self.stack.clear();
                    self.move_by(0.0, dy, out)?;
                }
                5 => {
                    let (dx, dy) = (self.arg(2), self.arg(1));
                    self.stack.clear();
                    self.line_by(dx, dy, out)?;
                }
                6 => {
                    let dx = self.arg(1);
                    self.stack.clear();
                    self.line_by(dx, 0.0, out)?;
                }
                7 => {
                    let dy = self.arg(1);
                    self.stack.clear();
                    self.line_by(0.0, dy, out)?;
                }
                8 => {
                    let d = [self.arg(6), self.arg(5), self.arg(4), self.arg(3), self.arg(2), self.arg(1)];
                    self.stack.clear();
                    self.curve_by(d, out)?;
                }
                9 => out.close()?,
                10 => {
                    let index = self.pop();
                    if depth >= MAX_SUBR_DEPTH {
                        return Err("subroutines nested too deep");
                    }
                    let font = self.font;
                    let sub = usize::try_from(index as i64).ok().and_then(|i| font.subr(i)).ok_or("a subroutine is missing or cannot be read")?;
                    // A call costs the bytes of the subroutine, even if it returns at once.
                    self.ticks = self.ticks.saturating_add(sub.len());
                    if self.ticks > MAX_TICKS {
                        return Err("the charstring runs too long");
                    }
                    if let Flow::End = self.exec(sub, depth + 1, out)? {
                        return Ok(Flow::End);
                    }
                }
                11 => return Ok(Flow::Return),
                13 => {
                    let (sbx, _wx) = (self.arg(2), self.arg(1));
                    self.stack.clear();
                    self.sbx = sbx;
                    self.x = sbx;
                    self.y = 0.0;
                }
                14 => {
                    out.close()?;
                    return Ok(Flow::End);
                }
                21 => {
                    let (dx, dy) = (self.arg(2), self.arg(1));
                    self.stack.clear();
                    self.move_by(dx, dy, out)?;
                }
                22 => {
                    let dx = self.arg(1);
                    self.stack.clear();
                    self.move_by(dx, 0.0, out)?;
                }
                30 => {
                    let d = [0.0, self.arg(4), self.arg(3), self.arg(2), self.arg(1), 0.0];
                    self.stack.clear();
                    self.curve_by(d, out)?;
                }
                31 => {
                    let d = [self.arg(4), 0.0, self.arg(3), self.arg(2), 0.0, self.arg(1)];
                    self.stack.clear();
                    self.curve_by(d, out)?;
                }
                12 => {
                    let b1 = *cs.get(pos).ok_or("truncated charstring")?;
                    pos += 1;
                    if let Flow::End = self.escape(b1, out)? {
                        return Ok(Flow::End);
                    }
                }
                _ => self.stack.clear(),
            }
        }
        Ok(Flow::Go)
    }

    fn escape(&mut self, op: u8, out: &mut Builder) -> Res<Flow> {
        match op {
            // dotsection, vstem3, hstem3
            0..=2 => self.stack.clear(),
            6 => {
                // seac: asb adx ady bchar achar
                let (asb, adx, ady, bchar, achar) = (self.arg(5), self.arg(4), self.arg(3), self.arg(2), self.arg(1));
                self.stack.clear();
                if self.seac {
                    return Err("seac inside seac");
                }
                let find = |code: f64| -> Option<usize> {
                    let name = fontprog::standard_encoding_name(u8::try_from(code as i64).ok()?)?;
                    self.font.by_name.get(name).copied()
                };
                let (base, accent) = (find(bchar).ok_or("seac: no base glyph")?, find(achar).ok_or("seac: no accent glyph")?);
                self.seac = true;
                let (sbx, ox, oy) = (self.sbx, self.ox, self.oy);
                self.glyph(base, out)?;
                self.ox = ox + sbx + adx - asb;
                self.oy = oy + ady;
                let result = self.glyph(accent, out);
                self.ox = ox;
                self.oy = oy;
                result?;
                out.close()?;
                return Ok(Flow::End);
            }
            7 => {
                // sbw: sbx sby wx wy
                let (sbx, sby) = (self.arg(4), self.arg(3));
                self.stack.clear();
                self.sbx = sbx;
                self.x = sbx;
                self.y = sby;
            }
            12 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(if b == 0.0 { 0.0 } else { a / b })?;
            }
            16 => {
                // callothersubr: arg1 .. argn n othersubr#
                let which = self.pop() as i64;
                let n = usize::try_from(self.pop() as i64).unwrap_or(0).min(self.stack.len());
                let args: Vec<f64> = self.stack.split_off(self.stack.len() - n);
                match which {
                    1 => self.flex = Some(Vec::new()),
                    2 => {
                        let here = (self.ox + self.x, self.oy + self.y);
                        if let Some(points) = self.flex.as_mut()
                            && points.len() < MAX_PS
                        {
                            points.push(here);
                        }
                    }
                    0 => {
                        // End of flex: the first of seven points is the reference point; the other six are two curves.
                        let points = self.flex.take().unwrap_or_default();
                        if let [_, p1, p2, p3, p4, p5, p6, ..] = points.as_slice() {
                            out.cubic_to(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1)?;
                            out.cubic_to(p4.0, p4.1, p5.0, p5.1, p6.0, p6.1)?;
                        }
                        if let [_, x, y] = args.as_slice() {
                            self.ps.push(*y);
                            self.ps.push(*x);
                        }
                    }
                    _ => {
                        // Hint replacement (3) and unknown procedures: the arguments come back through `pop`.
                        for &a in args.iter().rev() {
                            if self.ps.len() < MAX_PS {
                                self.ps.push(a);
                            }
                        }
                    }
                }
            }
            17 => {
                let v = self.ps.pop().unwrap_or(0.0);
                self.push(v)?;
            }
            33 => {
                let (x, y) = (self.arg(2), self.arg(1));
                self.stack.clear();
                self.x = x;
                self.y = y;
            }
            _ => self.stack.clear(),
        }
        Ok(Flow::Go)
    }
}
