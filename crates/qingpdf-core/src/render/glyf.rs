//! TrueType outlines: the `glyf`, `loca`, `head`, `maxp`, `hhea`, `hmtx` and `cmap` tables of an
//! sfnt file (OpenType specification; ISO 32000-1 9.9 and 9.6.6.4 for how a PDF picks glyphs).
//!
//! Every offset and count read from the file is checked: a table that does not fit makes the
//! font an error, a glyph that does not fit is an error for that glyph only. Composite glyphs are
//! limited in depth and in the number of components, and a glyph that includes itself is an error.
//! The `glyf` table of a system font stays in its file and is read a glyph at a time; everything else
//! is kept in memory.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::text::fontprog::{self, Post, Sub, u8_at, u16_at};

use super::outline::{Builder, Res};

mod hint;
mod tricky;

use hint::ProgSrc;

/// Composite glyphs may nest this deep.
const MAX_COMPOSITE_DEPTH: usize = 8;
/// Components one glyph may have in all, however they nest.
const MAX_COMPONENTS: usize = 256;
/// Points one glyph may have in all (TrueType's own limit is 65,535).
const MAX_POINTS: usize = 40_000;
/// The largest glyph record read from a file, and the largest small table.
const MAX_GLYPH_RECORD: usize = 1 << 20;
const MAX_TABLE: usize = 32 << 20;
/// Glyph names looked at when a font is searched by name.
const MAX_NAMED_GLYPHS: usize = 30_000;

// --- reading from a file ---------------------------------------------------------------------------

#[cfg(windows)]
fn pos_read(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, off)
}
#[cfg(unix)]
fn pos_read(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, off)
}
#[cfg(not(any(windows, unix)))]
fn pos_read(_: &File, _: &mut [u8], _: u64) -> std::io::Result<usize> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

fn read_at(f: &File, off: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut done = 0usize;
    while done < len {
        let n = pos_read(f, buf.get_mut(done..)?, off.checked_add(done as u64)?).ok()?;
        if n == 0 {
            return None;
        }
        done += n;
    }
    Some(buf)
}

/// Where the glyph records are.
enum Glyf {
    Memory { data: Arc<Vec<u8>>, off: usize, len: usize },
    File { file: Arc<File>, off: u64, len: usize },
}

impl Glyf {
    fn len(&self) -> usize {
        match self {
            Glyf::Memory { len, .. } | Glyf::File { len, .. } => *len,
        }
    }

    /// Bytes `a..b` of the table.
    fn slice(&self, a: usize, b: usize) -> Option<Cow<'_, [u8]>> {
        if a > b || b > self.len() || b - a > MAX_GLYPH_RECORD {
            return None;
        }
        match self {
            Glyf::Memory { data, off, .. } => data.get(off.checked_add(a)?..off.checked_add(b)?).map(Cow::Borrowed),
            Glyf::File { file, off, .. } => read_at(file, off.checked_add(a as u64)?, b - a).map(Cow::Owned),
        }
    }
}

// --- cmap ----------------------------------------------------------------------------------------------

/// The `cmap` table with the subtables a PDF reader uses (9.6.6.4).
pub(crate) struct Cmap {
    data: Vec<u8>,
    /// Unicode subtables, the best first.
    unicode: Vec<usize>,
    symbol: Option<usize>,
    mac: Option<usize>,
}

impl Cmap {
    pub fn new(data: Vec<u8>) -> Cmap {
        let subs: Vec<Sub> = fontprog::cmap_subtables(&data);
        let mut unicode: Vec<(u8, usize)> = subs
            .iter()
            .filter_map(|s| {
                let rank = match (s.platform, s.encoding) {
                    (3, 10) => 0,
                    (0, 4 | 6) => 1,
                    (3, 1) => 2,
                    (0, _) => 3,
                    _ => return None,
                };
                Some((rank, s.off))
            })
            .collect();
        unicode.sort_by_key(|e| e.0);
        let find = |p: u16, e: u16| subs.iter().find(|s| s.platform == p && s.encoding == e).map(|s| s.off);
        let (symbol, mac) = (find(3, 0), find(1, 0));
        Cmap { unicode: unicode.into_iter().map(|e| e.1).collect(), symbol, mac, data }
    }

    /// The glyph of a Unicode value.
    pub fn unicode(&self, cp: u32) -> Option<u32> {
        self.unicode.iter().find_map(|&off| fontprog::cmap_lookup(&self.data, off, cp)).map(u32::from)
    }

    /// The glyph of a code in the (3,0) subtable: the code itself or in the ranges at 0xF000, 0xF100, 0xF200.
    pub fn symbol(&self, code: u32) -> Option<u32> {
        let off = self.symbol?;
        [0, 0xF000, 0xF100, 0xF200].iter().find_map(|base| fontprog::cmap_lookup(&self.data, off, base + code)).map(u32::from)
    }

    /// The glyph of a code in the (1,0) subtable.
    pub fn mac(&self, code: u32) -> Option<u32> {
        fontprog::cmap_lookup(&self.data, self.mac?, code).map(u32::from)
    }

    pub fn bytes(&self) -> usize {
        self.data.len()
    }
}

// --- the font ----------------------------------------------------------------------------------------

pub(crate) struct TtFont {
    units_per_em: f64,
    num_glyphs: usize,
    loca: Vec<u8>,
    loca_long: bool,
    glyf: Glyf,
    hmtx: Vec<u8>,
    num_hmetrics: usize,
    pub cmap: Option<Cmap>,
    post: Option<Vec<u8>>,
    names: OnceLock<HashMap<String, u32>>,
    /// The `maxp` table (the limits of the instructions), the ascender and the descender of `hhea` (the phantom points).
    maxp: Vec<u8>,
    ascender: i16,
    descender: i16,
    /// Where the `cvt `, `fpgm` and `prep` tables are, and whether the font needs them run (3b2).
    prog: ProgSrc,
    tricky: bool,
    /// The state `fpgm` and `prep` leave, made at the first hinted glyph (or why it could not be made).
    hint: OnceLock<Result<super::ttvm::Globals, &'static str>>,
}

impl TtFont {
    /// A font held in memory (an embedded program), font number `face` of a collection. The
    /// `post` table is kept for glyph names.
    pub fn from_memory(data: Arc<Vec<u8>>, face: usize) -> Res<TtFont> {
        if !fontprog::is_sfnt(&data) {
            return Err("not a TrueType font");
        }
        let (goff, glen) = fontprog::sfnt_table_range(&data, face, b"glyf").ok_or("no glyf table")?;
        let mut get = |tag: &[u8; 4]| -> Option<Vec<u8>> { fontprog::sfnt_table_face(&data, face, tag).map(<[u8]>::to_vec) };
        let mut font = TtFont::build(&mut get, Glyf::Memory { data: data.clone(), off: goff, len: glen }, true)?;
        let range = |tag: &[u8; 4]| fontprog::sfnt_table_range(&data, face, tag).unwrap_or((0, 0));
        let (cvt, fpgm, prep) = (range(b"cvt "), range(b"fpgm"), range(b"prep"));
        let slice = |r: (usize, usize)| data.get(r.0..r.0.saturating_add(r.1)).unwrap_or_default();
        font.tricky = tricky::tables_are_tricky(slice(cvt), slice(fpgm), slice(prep)) || get(b"name").is_some_and(|n| tricky::name_table_is_tricky(&n));
        font.prog = ProgSrc::Memory { data: data.clone(), cvt, fpgm, prep };
        Ok(font)
    }

    /// A font file on disk: only the small tables are read now, the glyph records when they are needed.
    pub fn from_file(path: &Path, face: usize) -> Res<TtFont> {
        let file = File::open(path).map_err(|_| "cannot open the font file")?;
        let size = file.metadata().map_err(|_| "cannot read the font file")?.len();
        let head = read_at(&file, 0, 12).ok_or("short font file")?;
        let dir = if head.get(..4) == Some(b"ttcf") {
            let count = u32::from_be_bytes(head.get(8..12).and_then(|b| b.try_into().ok()).ok_or("short font file")?) as usize;
            if face >= count.min(64) {
                return Err("no such font in the collection");
            }
            let rec = read_at(&file, 12 + 4 * face as u64, 4).ok_or("short font file")?;
            u32::from_be_bytes(rec.as_slice().try_into().map_err(|_| "short font file")?) as u64
        } else if matches!(head.get(..4), Some([0, 1, 0, 0] | b"true" | b"OTTO")) && face == 0 {
            0
        } else {
            return Err("not a TrueType font");
        };
        let top = read_at(&file, dir, 12).ok_or("short font file")?;
        let n = usize::from(u16_at(&top, 4).ok_or("short font file")?).min(256);
        let table_dir = read_at(&file, dir + 12, n * 16).ok_or("short font file")?;
        let range = |tag: &[u8; 4]| -> Option<(u64, usize)> {
            for i in 0..n {
                let rec = table_dir.get(i * 16..i * 16 + 16)?;
                if rec.get(..4)? == tag {
                    let off = u64::from(u32::from_be_bytes(rec.get(8..12)?.try_into().ok()?));
                    let len = u32::from_be_bytes(rec.get(12..16)?.try_into().ok()?) as usize;
                    return (off.checked_add(len as u64)? <= size).then_some((off, len));
                }
            }
            None
        };
        let (goff, glen) = range(b"glyf").ok_or("no glyf table")?;
        let mut get = |tag: &[u8; 4]| -> Option<Vec<u8>> {
            let (off, len) = range(tag)?;
            if len > MAX_TABLE {
                return None;
            }
            read_at(&file, off, len)
        };
        let handle = Arc::new(file.try_clone().map_err(|_| "cannot read the font file")?);
        let mut font = TtFont::build(&mut get, Glyf::File { file: handle, off: goff, len: glen }, false)?;
        // A system font is tricky only by its tables (the old MingLiU of Windows before 7 is); they are kept when it is.
        let (cvt, fpgm, prep) = (get(b"cvt ").unwrap_or_default(), get(b"fpgm").unwrap_or_default(), get(b"prep").unwrap_or_default());
        if tricky::tables_are_tricky(&cvt, &fpgm, &prep) {
            font.tricky = true;
            font.prog = ProgSrc::Owned { cvt, fpgm, prep };
        }
        Ok(font)
    }

    fn build(get: &mut dyn FnMut(&[u8; 4]) -> Option<Vec<u8>>, glyf: Glyf, keep_post: bool) -> Res<TtFont> {
        let head = get(b"head").ok_or("no head table")?;
        let units_per_em = f64::from(u16_at(&head, 18).ok_or("bad head table")?);
        if !(16.0..=16384.0).contains(&units_per_em) {
            return Err("bad units per em");
        }
        let loca_long = u16_at(&head, 50).ok_or("bad head table")? != 0;
        let maxp = get(b"maxp").ok_or("no maxp table")?;
        let declared = usize::from(u16_at(&maxp, 4).ok_or("bad maxp table")?);
        let loca = get(b"loca").ok_or("no loca table")?;
        let entries = loca.len() / if loca_long { 4 } else { 2 };
        let num_glyphs = declared.min(entries.saturating_sub(1));
        let hmtx = get(b"hmtx").unwrap_or_default();
        let hhea = get(b"hhea").unwrap_or_default();
        let num_hmetrics = u16_at(&hhea, 34).map_or(0, usize::from).min(hmtx.len() / 4);
        let (ascender, descender) = (u16_at(&hhea, 4).unwrap_or(0) as i16, u16_at(&hhea, 6).unwrap_or(0) as i16);
        let cmap = get(b"cmap").map(Cmap::new);
        let post = if keep_post { get(b"post") } else { None };
        Ok(TtFont {
            units_per_em,
            num_glyphs,
            loca,
            loca_long,
            glyf,
            hmtx,
            num_hmetrics,
            cmap,
            post,
            names: OnceLock::new(),
            maxp,
            ascender,
            descender,
            prog: ProgSrc::None,
            tricky: false,
            hint: OnceLock::new(),
        })
    }

    #[cfg(test)]
    pub fn num_glyphs_for_test(&self) -> usize {
        self.num_glyphs
    }

    /// Bytes this font keeps in memory.
    pub fn memory(&self) -> usize {
        self.loca.len() + self.hmtx.len() + self.post.as_ref().map_or(0, Vec::len) + self.cmap.as_ref().map_or(0, Cmap::bytes) + self.prog.bytes() + 256
    }

    /// The advance of a glyph in em units.
    pub fn advance(&self, gid: u32) -> Option<f32> {
        if self.num_hmetrics == 0 {
            return None;
        }
        let i = usize::try_from(gid).ok()?.min(self.num_hmetrics - 1);
        Some((f64::from(u16_at(&self.hmtx, i * 4)?) / self.units_per_em) as f32)
    }

    /// The glyph of a PostScript glyph name, from the `post` table.
    pub fn glyph_by_name(&self, name: &str) -> Option<u32> {
        let map = self.names.get_or_init(|| {
            let mut map = HashMap::new();
            if let Some(post) = self.post.as_deref().and_then(Post::parse) {
                for gid in 0..self.num_glyphs.min(MAX_NAMED_GLYPHS) {
                    if let Some(n) = u16::try_from(gid).ok().and_then(|g| post.name(g)) {
                        map.entry(n).or_insert(u32::try_from(gid).unwrap_or(0));
                    }
                }
            }
            map
        });
        map.get(name).copied()
    }

    /// Where the record of a glyph is in `glyf`: `None` for a record that does not fit, an empty range for no outline.
    fn record(&self, gid: usize) -> Res<(usize, usize)> {
        if gid >= self.num_glyphs {
            return Err("glyph number out of range");
        }
        let at = |i: usize| -> Option<usize> {
            if self.loca_long {
                usize::try_from(u32::from_be_bytes(self.loca.get(i * 4..i * 4 + 4)?.try_into().ok()?)).ok()
            } else {
                Some(usize::from(u16_at(&self.loca, i * 2)?) * 2)
            }
        };
        let (a, b) = (at(gid).ok_or("bad loca table")?, at(gid + 1).ok_or("bad loca table")?);
        if a > b || b > self.glyf.len() {
            return Err("a glyph is outside the glyf table");
        }
        Ok((a, b))
    }

    /// Draw glyph `gid` into `out`.
    pub fn outline(&self, gid: u32, out: &mut Builder) -> Res<()> {
        let mut contours = Contours::default();
        let mut budget = MAX_COMPONENTS;
        let mut stack = Vec::new();
        let loaded = usize::try_from(gid).map_err(|_| "glyph number out of range").and_then(|g| self.load(g, 0, &mut budget, &mut stack, &mut contours));
        out.charge(contours.work);
        loaded?;
        contours.emit(out)
    }

    fn load(&self, gid: usize, depth: usize, budget: &mut usize, stack: &mut Vec<usize>, out: &mut Contours) -> Res<()> {
        if depth > MAX_COMPOSITE_DEPTH {
            return Err("composite glyphs nested too deep");
        }
        if stack.contains(&gid) {
            return Err("a composite glyph includes itself");
        }
        let (a, b) = self.record(gid)?;
        if a == b {
            return Ok(());
        }
        let rec = self.glyf.slice(a, b).ok_or("a glyph record cannot be read")?;
        let n = i16::from_be_bytes(u16_at(&rec, 0).ok_or("truncated glyph")?.to_be_bytes());
        if n >= 0 {
            return simple_glyph(&rec, usize::try_from(n).map_err(|_| "bad contour count")?, out);
        }
        // A composite glyph (OpenType, glyf): components with a transform each.
        let mut pos = 10usize;
        loop {
            if *budget == 0 {
                return Err("a glyph has too many components");
            }
            *budget -= 1;
            let comp = parse_component(&rec, &mut pos)?;
            let (flags, child) = (comp.flags, comp.child);
            let (dx, dy) = if flags & 2 != 0 { (f64::from(comp.arg1), f64::from(comp.arg2)) } else { (0.0, 0.0) };
            // x' = a x + c y + e, y' = b x + d y + f
            let [a, b, c, d] = comp.matrix;
            let (dx, dy) = if flags & 0x800 != 0 && flags & 0x1000 == 0 { (a * dx + c * dy, b * dx + d * dy) } else { (dx, dy) };
            let start = out.pts.len();
            stack.push(gid);
            let loaded = self.load(child, depth + 1, budget, stack, out);
            stack.pop();
            loaded?;
            // The component is moved point by point.
            out.work = out.work.saturating_add(out.pts.len().saturating_sub(start) + 1);
            for p in out.pts.get_mut(start..).unwrap_or_default() {
                let (x, y) = (f64::from(p.0), f64::from(p.1));
                *p = ((a * x + c * y + dx) as f32, (b * x + d * y + dy) as f32, p.2);
            }
            if flags & 0x20 == 0 {
                return Ok(());
            }
        }
    }
}

/// One component of a composite glyph (OpenType, glyf): its flags, glyph, arguments and 2 x 2 matrix.
struct Comp {
    flags: u16,
    child: usize,
    /// The offset (font units) when ARGS_ARE_XY_VALUES, else the point numbers (in the glyph so far, in the component).
    arg1: i32,
    arg2: i32,
    /// [a, b, c, d]: x' = a x + c y, y' = b x + d y.
    matrix: [f64; 4],
}

/// Read the component at `pos` and move `pos` past it.
fn parse_component(rec: &[u8], pos: &mut usize) -> Res<Comp> {
    const CUT: &str = "truncated composite glyph";
    let flags = u16_at(rec, *pos).ok_or(CUT)?;
    let child = usize::from(u16_at(rec, *pos + 2).ok_or(CUT)?);
    *pos += 4;
    let xy = flags & 2 != 0;
    let (arg1, arg2) = if flags & 1 != 0 {
        let (x, y) = (u16_at(rec, *pos).ok_or(CUT)?, u16_at(rec, *pos + 2).ok_or(CUT)?);
        *pos += 4;
        if xy { (i32::from(x as i16), i32::from(y as i16)) } else { (i32::from(x), i32::from(y)) }
    } else {
        let (x, y) = (u8_at(rec, *pos).ok_or(CUT)?, u8_at(rec, *pos + 1).ok_or(CUT)?);
        *pos += 2;
        if xy { (i32::from(x as i8), i32::from(y as i8)) } else { (i32::from(x), i32::from(y)) }
    };
    let f2dot14 = |p: usize| -> Res<f64> { Ok(f64::from(u16_at(rec, p).ok_or(CUT)? as i16) / 16384.0) };
    let mut matrix = [1.0, 0.0, 0.0, 1.0];
    if flags & 8 != 0 {
        let a = f2dot14(*pos)?;
        matrix = [a, 0.0, 0.0, a];
        *pos += 2;
    } else if flags & 0x40 != 0 {
        matrix = [f2dot14(*pos)?, 0.0, 0.0, f2dot14(*pos + 2)?];
        *pos += 4;
    } else if flags & 0x80 != 0 {
        matrix = [f2dot14(*pos)?, f2dot14(*pos + 2)?, f2dot14(*pos + 4)?, f2dot14(*pos + 6)?];
        *pos += 8;
    }
    Ok(Comp { flags, child, arg1, arg2, matrix })
}

/// The contours of a glyph: points (x, y, on the curve) and where each contour ends.
#[derive(Default)]
struct Contours {
    pts: Vec<(f32, f32, bool)>,
    ends: Vec<usize>,
    /// Points and contours read, and points moved: the work done, for the page's budget.
    work: usize,
}

fn simple_glyph(rec: &[u8], contours: usize, out: &mut Contours) -> Res<()> {
    const BAD: &str = "truncated glyph";
    if contours == 0 {
        return Ok(());
    }
    let last = usize::from(u16_at(rec, 10 + 2 * (contours - 1)).ok_or(BAD)?);
    let count = last + 1;
    if out.pts.len() + count > MAX_POINTS || out.ends.len() + contours > MAX_POINTS {
        return Err("a glyph has too many points");
    }
    out.work = out.work.saturating_add(count + contours);
    let base = out.pts.len();
    let mut prev_end = None::<usize>;
    for i in 0..contours {
        let end = usize::from(u16_at(rec, 10 + 2 * i).ok_or(BAD)?);
        if prev_end.is_some_and(|p| end < p) || end > last {
            return Err("the contour ends of a glyph are not in order");
        }
        prev_end = Some(end);
        out.ends.push(base + end + 1);
    }
    let ilen = usize::from(u16_at(rec, 10 + 2 * contours).ok_or(BAD)?);
    let mut pos = 12 + 2 * contours + ilen;
    let mut flags: Vec<u8> = Vec::with_capacity(count);
    while flags.len() < count {
        let f = u8_at(rec, pos).ok_or(BAD)?;
        pos += 1;
        flags.push(f);
        if f & 8 != 0 {
            let repeat = usize::from(u8_at(rec, pos).ok_or(BAD)?);
            pos += 1;
            for _ in 0..repeat.min(count - flags.len()) {
                flags.push(f);
            }
        }
    }
    let coords = |short: u8, same: u8, pos: &mut usize| -> Res<Vec<f32>> {
        let mut v = Vec::with_capacity(count);
        let mut acc = 0i32;
        for &f in &flags {
            if f & short != 0 {
                let d = i32::from(u8_at(rec, *pos).ok_or(BAD)?);
                *pos += 1;
                acc += if f & same != 0 { d } else { -d };
            } else if f & same == 0 {
                acc += i32::from(u16_at(rec, *pos).ok_or(BAD)? as i16);
                *pos += 2;
            }
            v.push(acc as f32);
        }
        Ok(v)
    };
    let xs = coords(2, 16, &mut pos)?;
    let ys = coords(4, 32, &mut pos)?;
    for ((x, y), f) in xs.into_iter().zip(ys).zip(flags) {
        out.pts.push((x, y, f & 1 != 0));
    }
    Ok(())
}

impl Contours {
    /// Quadratic outlines from the points (the off-curve points between two others have an implied
    /// on-curve point halfway).
    fn emit(&self, b: &mut Builder) -> Res<()> {
        let mut from = 0usize;
        for &end in &self.ends {
            let pts = self.pts.get(from..end).ok_or("bad contour")?;
            from = end;
            let n = pts.len();
            if n < 2 {
                continue;
            }
            let mid = |p: (f32, f32, bool), q: (f32, f32, bool)| (f64::from(p.0 + q.0) / 2.0, f64::from(p.1 + q.1) / 2.0);
            let (start, first, count) = match pts.iter().position(|p| p.2) {
                Some(k) => {
                    let p = pts.get(k).ok_or("bad contour")?;
                    ((f64::from(p.0), f64::from(p.1)), k + 1, n - 1)
                }
                None => (mid(*pts.get(n - 1).ok_or("bad contour")?, *pts.first().ok_or("bad contour")?), 0, n),
            };
            b.move_to(start.0, start.1)?;
            let mut ctrl: Option<(f64, f64)> = None;
            for i in 0..count {
                let p = *pts.get((first + i) % n).ok_or("bad contour")?;
                let (x, y) = (f64::from(p.0), f64::from(p.1));
                if p.2 {
                    match ctrl.take() {
                        Some((cx, cy)) => b.quad_to(cx, cy, x, y)?,
                        None => b.line_to(x, y)?,
                    }
                } else {
                    if let Some((cx, cy)) = ctrl {
                        let (mx, my) = ((cx + x) / 2.0, (cy + y) / 2.0);
                        b.quad_to(cx, cy, mx, my)?;
                    }
                    ctrl = Some((x, y));
                }
            }
            if let Some((cx, cy)) = ctrl {
                b.quad_to(cx, cy, start.0, start.1)?;
            }
            b.close()?;
        }
        Ok(())
    }
}

impl TtFont {
    /// The matrix from font units to em units.
    pub fn em_matrix(&self) -> [f64; 6] {
        let s = 1.0 / self.units_per_em;
        [s, 0.0, 0.0, s, 0.0, 0.0]
    }
}
