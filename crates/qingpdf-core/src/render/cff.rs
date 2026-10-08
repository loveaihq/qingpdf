//! CFF outlines: Type 2 charstrings (Adobe Technical Note #5177) in a CFF font (Technical Note
//! #5176), bare (`/FontFile3 /Type1C`, `/CIDFontType0C`) or inside an OpenType file (`/OpenType`).
//!
//! The container (INDEX, DICT, charset, encoding) comes from `text::fontprog`. A charstring may
//! run at most [`MAX_TICKS`] operators and operands, call subroutines [`MAX_SUBR_DEPTH`] deep (the
//! limit of the Type 2 specification), and keep [`MAX_STACK`] numbers on its stack; the outline has the
//! segment limit of [`Builder`].

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::text::data;
use crate::text::fontprog::{self, Index, Res, u8_at, u16_at};

use super::glyf::Cmap;
use super::outline::Builder;

/// Operators and operands one glyph may run, subroutines included.
const MAX_TICKS: usize = 200_000;
/// Subroutine nesting (Type 2: 10).
const MAX_SUBR_DEPTH: usize = 10;
/// The argument stack (Type 2: 48; some fonts go further).
const MAX_STACK: usize = 96;
/// The `put`/`get` registers.
const TRANSIENT: usize = 32;

struct Fd {
    subrs: Option<Index>,
    matrix: Option<[f64; 6]>,
}

pub(crate) struct CffFont {
    data: Arc<Vec<u8>>,
    off: usize,
    len: usize,
    charstrings: Index,
    gsubrs: Index,
    fds: Vec<Fd>,
    /// The font dict of each glyph (empty: all use the first).
    fd_select: Vec<u8>,
    /// Per glyph: the string id, or for a CID-keyed font the CID.
    charset: Vec<u16>,
    cid_keyed: bool,
    strings: Index,
    top_matrix: Option<[f64; 6]>,
    encoding: usize,
    /// The `cmap` of the OpenType file the font came in, if any.
    pub cmap: Option<Cmap>,
    by_sid: OnceLock<HashMap<u16, u32>>,
    by_name: OnceLock<HashMap<String, u32>>,
    by_unicode: OnceLock<HashMap<u32, u32>>,
    by_cid: OnceLock<HashMap<u32, u32>>,
    builtin: OnceLock<Vec<u32>>,
}

fn matrix_of(args: &[f64]) -> Option<[f64; 6]> {
    let m: [f64; 6] = args.get(..6)?.try_into().ok()?;
    (m.iter().all(|v| v.is_finite()) && (m[0] * m[3] - m[1] * m[2]).abs() > 1e-12).then_some(m)
}

fn mul(a: &[f64; 6], b: &[f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

impl CffFont {
    /// A CFF table at `off..off + len` of `data`.
    pub fn parse(data: Arc<Vec<u8>>, off: usize, len: usize, cmap: Option<Cmap>) -> Res<CffFont> {
        let d = data.get(off..off.checked_add(len).ok_or("bad CFF table")?).ok_or("bad CFF table")?;
        if u8_at(d, 0) != Some(1) {
            return Err("not a CFF font (version 1)");
        }
        let header = usize::from(u8_at(d, 2).ok_or("bad CFF header")?);
        let names = Index::parse(d, header)?;
        let tops = Index::parse(d, names.end)?;
        let strings = Index::parse(d, tops.end)?;
        let gsubrs = Index::parse(d, strings.end)?;
        let top = fontprog::dict_operands(tops.get(d, 0).ok_or("no Top DICT")?)?;
        let find = |ops: &[(u16, Vec<f64>)], op: u16| ops.iter().find(|(o, _)| *o == op).map(|(_, a)| a.clone());
        let offset = |args: Option<Vec<f64>>| args.and_then(|a| a.last().copied()).and_then(fontprog::offset_operand);
        let charstrings = Index::parse(d, offset(find(&top, 17)).ok_or("no CharStrings in the Top DICT")?)?;
        let n = charstrings.count;
        if n == 0 {
            return Err("a CFF font without glyphs");
        }
        let cid_keyed = find(&top, 1230).is_some();
        let charset_off = find(&top, 15).map_or(Some(0), |a| a.last().copied().and_then(fontprog::offset_operand)).ok_or("bad charset offset")?;
        // The predefined Expert charsets are not read; the glyphs then have no names.
        let charset = fontprog::charset(d, charset_off, n).unwrap_or_else(|_| (0..n).map(|i| u16::try_from(i).unwrap_or(0)).collect());
        let encoding = find(&top, 16).map_or(Some(0), |a| a.last().copied().and_then(fontprog::offset_operand)).ok_or("bad Encoding offset")?;
        let top_matrix = find(&top, 1207).and_then(|a| matrix_of(&a));

        // The Private DICT: local subroutines (the offset is relative to the Private DICT).
        let private_subrs = |args: &[f64]| -> Option<Index> {
            let (size, at) = (fontprog::offset_operand(*args.first()?)?, fontprog::offset_operand(*args.get(1)?)?);
            let dict = fontprog::dict_operands(d.get(at..at.checked_add(size)?)?).ok()?;
            let sub = dict.iter().find(|(o, _)| *o == 19)?.1.last().copied().and_then(fontprog::offset_operand)?;
            Index::parse(d, at.checked_add(sub)?).ok()
        };
        let mut fds = Vec::new();
        let mut fd_select = Vec::new();
        if cid_keyed {
            let array = Index::parse(d, offset(find(&top, 1236)).ok_or("no FDArray in a CID-keyed font")?)?;
            for i in 0..array.count.min(256) {
                let dict = fontprog::dict_operands(array.get(d, i).ok_or("bad FDArray")?)?;
                fds.push(Fd { subrs: find(&dict, 18).and_then(|a| private_subrs(&a)), matrix: find(&dict, 1207).and_then(|a| matrix_of(&a)) });
            }
            let at = offset(find(&top, 1237)).ok_or("no FDSelect in a CID-keyed font")?;
            fd_select = read_fd_select(d, at, n)?;
        } else {
            fds.push(Fd { subrs: find(&top, 18).and_then(|a| private_subrs(&a)), matrix: None });
        }
        if fds.is_empty() {
            return Err("a CFF font without font dicts");
        }
        Ok(CffFont {
            data,
            off,
            len,
            charstrings,
            gsubrs,
            fds,
            fd_select,
            charset,
            cid_keyed,
            strings,
            top_matrix,
            encoding,
            cmap,
            by_sid: OnceLock::new(),
            by_name: OnceLock::new(),
            by_unicode: OnceLock::new(),
            by_cid: OnceLock::new(),
            builtin: OnceLock::new(),
        })
    }

    fn d(&self) -> &[u8] {
        self.data.get(self.off..self.off + self.len).unwrap_or(&[])
    }

    pub fn num_glyphs(&self) -> usize {
        self.charstrings.count
    }

    fn fd_of(&self, gid: usize) -> &Fd {
        let i = if self.fd_select.is_empty() { 0 } else { usize::from(self.fd_select.get(gid).copied().unwrap_or(0)) };
        self.fds.get(i).or_else(|| self.fds.first()).unwrap_or(&NO_FD)
    }

    /// The matrix from charstring units to em units.
    fn matrix(&self, gid: usize) -> [f64; 6] {
        const DEFAULT: [f64; 6] = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
        match (self.top_matrix, self.fd_of(gid).matrix) {
            (None, None) => DEFAULT,
            (Some(t), None) => t,
            (None, Some(f)) => f,
            (Some(t), Some(f)) => {
                let m = mul(&f, &t);
                if m[0].abs() < 1e-5 { t } else { m }
            }
        }
    }

    // --- looking glyphs up ---

    fn sid_map(&self) -> &HashMap<u16, u32> {
        self.by_sid.get_or_init(|| {
            let mut map = HashMap::new();
            for (gid, &sid) in self.charset.iter().enumerate() {
                map.entry(sid).or_insert(u32::try_from(gid).unwrap_or(0));
            }
            map
        })
    }

    /// The glyph of a CID (a CID-keyed font), or for a font that is not CID-keyed the glyph with that number.
    pub fn gid_of_cid(&self, cid: u32) -> Option<u32> {
        if !self.cid_keyed {
            return ((cid as usize) < self.num_glyphs()).then_some(cid);
        }
        self.by_cid
            .get_or_init(|| {
                let mut map = HashMap::new();
                for (gid, &c) in self.charset.iter().enumerate() {
                    map.entry(u32::from(c)).or_insert(u32::try_from(gid).unwrap_or(0));
                }
                map
            })
            .get(&cid)
            .copied()
    }

    fn name_map(&self) -> &HashMap<String, u32> {
        self.by_name.get_or_init(|| {
            let mut map = HashMap::new();
            if !self.cid_keyed {
                for (gid, &sid) in self.charset.iter().enumerate() {
                    if let Some(name) = self.sid_name(sid) {
                        map.entry(name).or_insert(u32::try_from(gid).unwrap_or(0));
                    }
                }
            }
            map
        })
    }

    fn sid_name(&self, sid: u16) -> Option<String> {
        match usize::from(sid).checked_sub(391) {
            None => fontprog::cff_standard_string(usize::from(sid)).map(str::to_string),
            Some(i) => self.strings.get(self.d(), i).map(|b| String::from_utf8_lossy(b).into_owned()),
        }
    }

    pub fn gid_of_name(&self, name: &str) -> Option<u32> {
        self.name_map().get(name).copied().filter(|&g| g != 0 || name != ".notdef")
    }

    /// The glyph for a Unicode value, through the glyph names (and the `cmap` of an OpenType file).
    pub fn gid_of_unicode(&self, cp: u32) -> Option<u32> {
        let map = self.by_unicode.get_or_init(|| {
            let mut map: HashMap<u32, u32> = HashMap::new();
            for (name, &gid) in self.name_map() {
                if name == ".notdef" {
                    continue;
                }
                if let [one] = data::glyph_name_to_unicode(name).as_slice() {
                    let slot = map.entry(*one).or_insert(gid);
                    // Several glyphs for one character: the first in the font wins.
                    *slot = (*slot).min(gid);
                }
            }
            map
        });
        map.get(&cp).copied().or_else(|| self.cmap.as_ref().and_then(|c| c.unicode(cp)))
    }

    /// The glyph the font's own encoding gives a code.
    pub fn gid_of_code(&self, code: u8) -> Option<u32> {
        let table = self.builtin.get_or_init(|| {
            let mut table = vec![0u32; 256];
            match self.encoding {
                0 => {
                    for (code, slot) in table.iter_mut().enumerate() {
                        let sid = u8::try_from(code).ok().and_then(fontprog::standard_encoding_sid);
                        *slot = sid.and_then(|s| self.sid_map().get(&s).copied()).unwrap_or(0);
                    }
                }
                1 => {}
                at => {
                    if let Ok((gids, supplement)) = fontprog::cff_encoding(self.d(), at) {
                        for (slot, g) in table.iter_mut().zip(gids) {
                            *slot = u32::try_from(g).unwrap_or(0).min(u32::try_from(self.num_glyphs()).unwrap_or(0));
                        }
                        for (code, sid) in supplement {
                            if let (Some(slot), Some(&g)) = (table.get_mut(code), self.sid_map().get(&sid)) {
                                *slot = g;
                            }
                        }
                    }
                }
            }
            table
        });
        table.get(usize::from(code)).copied().filter(|&g| g != 0)
    }

    // --- outlines ---

    /// Draw glyph `gid` into `out` (made with [`CffFont::builder`]).
    pub fn outline(&self, gid: u32, out: &mut Builder) -> Res<()> {
        let gid = usize::try_from(gid).map_err(|_| "glyph number out of range")?;
        let mut run = Run { font: self, stack: Vec::new(), x: 0.0, y: 0.0, ox: 0.0, oy: 0.0, nstems: 0, ticks: 0, transient: [0.0; TRANSIENT], seac: false };
        let result = run.glyph(gid, out);
        out.charge(run.ticks);
        result?;
        out.close()
    }

    pub fn builder(&self, gid: u32) -> Builder {
        Builder::new(self.matrix(usize::try_from(gid).unwrap_or(0)))
    }
}

static NO_FD: Fd = Fd { subrs: None, matrix: None };

pub(super) fn read_fd_select(d: &[u8], at: usize, n: usize) -> Res<Vec<u8>> {
    const BAD: &str = "bad FDSelect in the CFF data";
    let mut out = vec![0u8; n];
    match u8_at(d, at).ok_or(BAD)? {
        0 => {
            let bytes = d.get(at + 1..at.checked_add(1 + n).ok_or(BAD)?).ok_or(BAD)?;
            out.copy_from_slice(bytes);
        }
        3 => {
            // Format 3 (5176 Table 20): ranges in rising order, the last one ends at a sentinel. One pass over the
            // glyphs: a range that does not rise is ignored, one past the glyphs is cut.
            let ranges = usize::from(u16_at(d, at + 1).ok_or(BAD)?);
            let mut done = 0usize;
            for i in 0..ranges {
                let rec = at + 3 + 3 * i;
                let (first, fd, next) = (usize::from(u16_at(d, rec).ok_or(BAD)?), u8_at(d, rec + 2).ok_or(BAD)?, usize::from(u16_at(d, rec + 3).ok_or(BAD)?));
                let (first, next) = (first.min(n), next.min(n));
                if first < done || next <= first {
                    continue;
                }
                for slot in out.get_mut(first..next).unwrap_or_default() {
                    *slot = fd;
                }
                done = next;
            }
        }
        _ => return Err(BAD),
    }
    Ok(out)
}

fn bias(count: usize) -> i64 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

struct Run<'a> {
    font: &'a CffFont,
    stack: Vec<f64>,
    x: f64,
    y: f64,
    /// The offset of the glyph being drawn (an accent placed by `seac`).
    ox: f64,
    oy: f64,
    nstems: usize,
    ticks: usize,
    transient: [f64; TRANSIENT],
    seac: bool,
}

enum Flow {
    Go,
    Return,
    End,
}

impl Run<'_> {
    fn glyph(&mut self, gid: usize, out: &mut Builder) -> Res<()> {
        let cs = self.font.charstrings.get(self.font.d(), gid).ok_or("glyph number out of range")?;
        self.stack.clear();
        self.x = 0.0;
        self.y = 0.0;
        self.nstems = 0;
        self.exec(cs, gid, 0, out)?;
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

    fn move_by(&mut self, dx: f64, dy: f64, out: &mut Builder) -> Res<()> {
        self.x += dx;
        self.y += dy;
        out.move_to(self.ox + self.x, self.oy + self.y)
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

    /// Stem hints take their arguments in pairs; an odd one first is the width.
    fn stems(&mut self) {
        self.nstems += self.stack.len() / 2;
        self.stack.clear();
    }

    fn exec(&mut self, cs: &[u8], gid: usize, depth: usize, out: &mut Builder) -> Res<Flow> {
        let mut pos = 0usize;
        while let Some(&b0) = cs.get(pos) {
            pos += 1;
            self.ticks += 1;
            if self.ticks > MAX_TICKS {
                return Err("the charstring runs too long");
            }
            match b0 {
                28 => {
                    let v = u16_at(cs, pos).ok_or("truncated charstring")?;
                    pos += 2;
                    self.push(f64::from(v as i16))?;
                }
                32..=246 => self.push(f64::from(b0) - 139.0)?,
                247..=250 => {
                    let b1 = u8_at(cs, pos).ok_or("truncated charstring")?;
                    pos += 1;
                    self.push((f64::from(b0) - 247.0) * 256.0 + f64::from(b1) + 108.0)?;
                }
                251..=254 => {
                    let b1 = u8_at(cs, pos).ok_or("truncated charstring")?;
                    pos += 1;
                    self.push(-(f64::from(b0) - 251.0) * 256.0 - f64::from(b1) - 108.0)?;
                }
                255 => {
                    let v = fontprog::u32_at(cs, pos).ok_or("truncated charstring")?;
                    pos += 4;
                    self.push(f64::from(v as i32) / 65536.0)?;
                }
                1 | 3 | 18 | 23 => self.stems(),
                19 | 20 => {
                    // hintmask, cntrmask: arguments left over are vertical stems; then the mask bytes.
                    self.stems();
                    pos = pos.checked_add(self.nstems.div_ceil(8)).ok_or("truncated charstring")?;
                    if pos > cs.len() {
                        return Err("truncated charstring");
                    }
                }
                21 => {
                    let n = self.stack.len();
                    let (dx, dy) = (self.stack.get(n.wrapping_sub(2)).copied().unwrap_or(0.0), self.stack.get(n.wrapping_sub(1)).copied().unwrap_or(0.0));
                    self.stack.clear();
                    self.move_by(dx, dy, out)?;
                }
                22 => {
                    let dx = self.stack.last().copied().unwrap_or(0.0);
                    self.stack.clear();
                    self.move_by(dx, 0.0, out)?;
                }
                4 => {
                    let dy = self.stack.last().copied().unwrap_or(0.0);
                    self.stack.clear();
                    self.move_by(0.0, dy, out)?;
                }
                5 => {
                    let args = std::mem::take(&mut self.stack);
                    for p in args.chunks_exact(2) {
                        if let [dx, dy] = *p {
                            self.line_by(dx, dy, out)?;
                        }
                    }
                }
                6 | 7 => {
                    let args = std::mem::take(&mut self.stack);
                    let mut horizontal = b0 == 6;
                    for &v in &args {
                        if horizontal { self.line_by(v, 0.0, out)? } else { self.line_by(0.0, v, out)? }
                        horizontal = !horizontal;
                    }
                }
                8 => {
                    let args = std::mem::take(&mut self.stack);
                    for c in args.chunks_exact(6) {
                        if let Ok(c) = <[f64; 6]>::try_from(c) {
                            self.curve_by(c, out)?;
                        }
                    }
                }
                24 => {
                    // rcurveline: curves, then a line.
                    let args = std::mem::take(&mut self.stack);
                    let curves = args.len().saturating_sub(2) / 6 * 6;
                    for c in args.get(..curves).unwrap_or_default().chunks_exact(6) {
                        if let Ok(c) = <[f64; 6]>::try_from(c) {
                            self.curve_by(c, out)?;
                        }
                    }
                    if let Some(&[dx, dy]) = args.get(curves..curves + 2).and_then(|s| <&[f64; 2]>::try_from(s).ok()) {
                        self.line_by(dx, dy, out)?;
                    }
                }
                25 => {
                    // rlinecurve: lines, then a curve.
                    let args = std::mem::take(&mut self.stack);
                    let lines = args.len().saturating_sub(6) / 2 * 2;
                    for p in args.get(..lines).unwrap_or_default().chunks_exact(2) {
                        if let [dx, dy] = *p {
                            self.line_by(dx, dy, out)?;
                        }
                    }
                    if let Some(&c) = args.get(lines..lines + 6).and_then(|s| <&[f64; 6]>::try_from(s).ok()) {
                        self.curve_by(c, out)?;
                    }
                }
                26 => {
                    // vvcurveto: dx1? {dya dxb dyb dyc}+
                    let args = std::mem::take(&mut self.stack);
                    let (mut dx1, rest) = match args.split_first() {
                        Some((&first, rest)) if args.len() % 4 == 1 => (first, rest),
                        _ => (0.0, args.as_slice()),
                    };
                    for c in rest.chunks_exact(4) {
                        if let [a, b, c, d] = *c {
                            self.curve_by([dx1, a, b, c, 0.0, d], out)?;
                        }
                        dx1 = 0.0;
                    }
                }
                27 => {
                    // hhcurveto: dy1? {dxa dxb dyb dxc}+
                    let args = std::mem::take(&mut self.stack);
                    let (mut dy1, rest) = match args.split_first() {
                        Some((&first, rest)) if args.len() % 4 == 1 => (first, rest),
                        _ => (0.0, args.as_slice()),
                    };
                    for c in rest.chunks_exact(4) {
                        if let [a, b, c, d] = *c {
                            self.curve_by([a, dy1, b, c, d, 0.0], out)?;
                        }
                        dy1 = 0.0;
                    }
                }
                30 | 31 => {
                    let args = std::mem::take(&mut self.stack);
                    let mut horizontal = b0 == 31;
                    let mut i = 0usize;
                    while let Some(&[p, q, r, t]) = args.get(i..i + 4).and_then(|s| <&[f64; 4]>::try_from(s).ok()) {
                        let extra = if args.len() - i == 5 { args.get(i + 4).copied().unwrap_or(0.0) } else { 0.0 };
                        if horizontal {
                            self.curve_by([p, 0.0, q, r, extra, t], out)?;
                        } else {
                            self.curve_by([0.0, p, q, r, t, extra], out)?;
                        }
                        horizontal = !horizontal;
                        i += 4;
                    }
                }
                10 | 29 => {
                    let n = if b0 == 10 { self.fonts_local_count(gid) } else { self.font.gsubrs.count };
                    let index = i64::from(self.pop() as i32).checked_add(bias(n)).ok_or("bad subroutine number")?;
                    if depth >= MAX_SUBR_DEPTH {
                        return Err("subroutines nested too deep");
                    }
                    let idx = usize::try_from(index).map_err(|_| "bad subroutine number")?;
                    let sub = if b0 == 10 {
                        self.font.fd_of(gid).subrs.as_ref().and_then(|s| s.get(self.font.d(), idx))
                    } else {
                        self.font.gsubrs.get(self.font.d(), idx)
                    };
                    let sub = sub.ok_or("a subroutine is missing")?;
                    match self.exec(sub, gid, depth + 1, out)? {
                        Flow::End => return Ok(Flow::End),
                        Flow::Return | Flow::Go => {}
                    }
                }
                11 => return Ok(Flow::Return),
                14 => {
                    self.endchar(out)?;
                    return Ok(Flow::End);
                }
                12 => {
                    let b1 = u8_at(cs, pos).ok_or("truncated charstring")?;
                    pos += 1;
                    self.escape(b1, out)?;
                }
                _ => self.stack.clear(),
            }
        }
        Ok(Flow::Go)
    }

    fn fonts_local_count(&self, gid: usize) -> usize {
        self.font.fd_of(gid).subrs.as_ref().map_or(0, |s| s.count)
    }

    /// `endchar`, with the four arguments of the old `seac` form (adx ady bchar achar): the base
    /// character and an accent, both by their StandardEncoding names.
    fn endchar(&mut self, out: &mut Builder) -> Res<()> {
        let n = self.stack.len();
        if n >= 4 && !self.seac {
            let &[adx, ady, bchar, achar] = self.stack.get(n - 4..).and_then(|s| <&[f64; 4]>::try_from(s).ok()).ok_or("bad seac")?;
            self.stack.clear();
            let find = |code: f64| -> Option<usize> {
                let sid = fontprog::standard_encoding_sid(code as u8)?;
                self.font.sid_map().get(&sid).map(|&g| g as usize)
            };
            let (base, accent) = (find(bchar).ok_or("seac: no base glyph")?, find(achar).ok_or("seac: no accent glyph")?);
            self.seac = true;
            let (ox, oy) = (self.ox, self.oy);
            self.glyph(base, out)?;
            self.ox = ox + adx;
            self.oy = oy + ady;
            let result = self.glyph(accent, out);
            self.ox = ox;
            self.oy = oy;
            result?;
        }
        self.stack.clear();
        out.close()
    }

    /// The two-byte operators: flex and the arithmetic ones.
    fn escape(&mut self, op: u8, out: &mut Builder) -> Res<()> {
        match op {
            34 => {
                // hflex: dx1 dx2 dy2 dx3 dx4 dx5 dx6
                let a = std::mem::take(&mut self.stack);
                if let [dx1, dx2, dy2, dx3, dx4, dx5, dx6] = a.as_slice() {
                    self.curve_by([*dx1, 0.0, *dx2, *dy2, *dx3, 0.0], out)?;
                    self.curve_by([*dx4, 0.0, *dx5, -*dy2, *dx6, 0.0], out)?;
                }
            }
            35 => {
                // flex: twelve numbers and the flex depth
                let a = std::mem::take(&mut self.stack);
                if let [a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, ..] = a.as_slice() {
                    self.curve_by([*a0, *a1, *a2, *a3, *a4, *a5], out)?;
                    self.curve_by([*a6, *a7, *a8, *a9, *a10, *a11], out)?;
                }
            }
            36 => {
                // hflex1: dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6
                let a = std::mem::take(&mut self.stack);
                if let [dx1, dy1, dx2, dy2, dx3, dx4, dx5, dy5, dx6] = a.as_slice() {
                    self.curve_by([*dx1, *dy1, *dx2, *dy2, *dx3, 0.0], out)?;
                    self.curve_by([*dx4, 0.0, *dx5, *dy5, *dx6, -(dy1 + dy2 + dy5)], out)?;
                }
            }
            37 => {
                // flex1: dx1 dy1 dx2 dy2 dx3 dy3 dx4 dy4 dx5 dy5 d6
                let a = std::mem::take(&mut self.stack);
                if let [dx1, dy1, dx2, dy2, dx3, dy3, dx4, dy4, dx5, dy5, d6] = a.as_slice() {
                    let (sx, sy) = (dx1 + dx2 + dx3 + dx4 + dx5, dy1 + dy2 + dy3 + dy4 + dy5);
                    let last = if sx.abs() > sy.abs() { (*d6, -sy) } else { (-sx, *d6) };
                    self.curve_by([*dx1, *dy1, *dx2, *dy2, *dx3, *dy3], out)?;
                    self.curve_by([*dx4, *dy4, *dx5, *dy5, last.0, last.1], out)?;
                }
            }
            3 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(f64::from(u8::from(a != 0.0 && b != 0.0)))?;
            }
            4 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(f64::from(u8::from(a != 0.0 || b != 0.0)))?;
            }
            5 => {
                let a = self.pop();
                self.push(f64::from(u8::from(a == 0.0)))?;
            }
            9 => {
                let a = self.pop();
                self.push(a.abs())?;
            }
            10 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(a + b)?;
            }
            11 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(a - b)?;
            }
            12 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(if b == 0.0 { 0.0 } else { a / b })?;
            }
            14 => {
                let a = self.pop();
                self.push(-a)?;
            }
            15 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(f64::from(u8::from(a == b)))?;
            }
            18 => {
                self.pop();
            }
            20 => {
                let (i, v) = (self.pop(), self.pop());
                if let Some(slot) = self.transient.get_mut(i as usize) {
                    *slot = v;
                }
            }
            21 => {
                let i = self.pop();
                let v = self.transient.get(i as usize).copied().unwrap_or(0.0);
                self.push(v)?;
            }
            22 => {
                let (v2, v1, s2, s1) = (self.pop(), self.pop(), self.pop(), self.pop());
                self.push(if v1 <= v2 { s1 } else { s2 })?;
            }
            23 => self.push(0.5)?,
            24 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(a * b)?;
            }
            26 => {
                let a = self.pop();
                self.push(a.max(0.0).sqrt())?;
            }
            27 => {
                let a = self.pop();
                self.push(a)?;
                self.push(a)?;
            }
            28 => {
                let (b, a) = (self.pop(), self.pop());
                self.push(b)?;
                self.push(a)?;
            }
            29 => {
                let i = self.pop();
                let n = self.stack.len();
                let at = if i < 0.0 { n.wrapping_sub(1) } else { n.wrapping_sub(1).wrapping_sub(i as usize) };
                let v = self.stack.get(at).copied().unwrap_or(0.0);
                self.push(v)?;
            }
            30 => {
                let (j, n) = (self.pop() as i64, self.pop() as i64);
                let len = self.stack.len();
                if n > 0 && (n as usize) <= len {
                    let n = n as usize;
                    let part = self.stack.get_mut(len - n..).ok_or("bad roll")?;
                    let shift = j.rem_euclid(n as i64) as usize;
                    part.rotate_right(shift);
                }
            }
            _ => self.stack.clear(),
        }
        Ok(())
    }
}
