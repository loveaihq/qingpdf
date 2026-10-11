//! Fonts put into a file (4b): a subset of a system TrueType font, holding only the glyphs the text uses, embedded as a
//! CIDFontType2 with `Identity-H` and `/CIDToGIDMap /Identity` (9.6.4, 9.7.4.2), with the widths in `/W` (9.7.4.3) and a
//! `/ToUnicode` CMap (9.10.3) that has every character used, so that text copied out of the file is what was typed.
//!
//! The subset is built from the font's own tables: glyph records are copied from `glyf` (the components of composite glyphs are
//! pulled in and renumbered; the hinting instructions are left out, so `cvt `, `fpgm` and `prep` are not needed), numbered from 1
//! in the order of the characters (a character has a glyph of its own even when another one shares its outline, so that the
//! `ToUnicode` entry of each is its own). The font's licence is kept: `OS/2` `fsType` says whether it may be embedded and whether
//! it may be subset (OpenType specification, `OS/2`).
//!
//! Limits: [`MAX_GLYPHS`] glyphs in one subset (components included), [`MAX_FONT_BYTES`] for the font program before it is
//! compressed, [`MAX_COMPONENT_VISITS`] components looked at; every glyph is charged to the work meter.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::sync::Arc;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::Ctx;
use crate::error::{Error, Result};
use crate::object::{Dict, ObjRef, Object, PdfString, Stream};
use crate::render::glyf::{TtFont, parse_component};
use crate::render::sysfont;
use crate::render::work::Work;
use crate::text::fontprog::u16_at;

/// Most glyphs one subset may have (components of composite glyphs included).
pub(crate) const MAX_GLYPHS: usize = 3_000;
/// Most bytes of font program (before compression) one subset may have.
pub(crate) const MAX_FONT_BYTES: usize = 3 << 20;
/// Most components of composite glyphs looked at for one subset.
const MAX_COMPONENT_VISITS: usize = 65_536;
/// What a glyph costs the work meter: looked up, copied, listed in the `/W` and `/ToUnicode` entries.
const GLYPH_COST: f64 = 40_000.0;

/// What a font is wanted for, from the name of the font a field's `/DA` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    Sans,
    Serif,
    Mono,
}

impl Look {
    pub(crate) fn of_name(name: &[u8]) -> Look {
        let lower: String = name.iter().map(|b| char::from(b.to_ascii_lowercase())).collect();
        if ["cour", "mono", "consolas", "typewriter"].iter().any(|k| lower.contains(k)) {
            Look::Mono
        } else if ["times", "tiro", "serif", "roman", "georgia", "song", "ming"].iter().any(|k| lower.contains(k)) && !lower.contains("sans") {
            Look::Serif
        } else {
            Look::Sans
        }
    }
}

/// System fonts to try, best first (file, number in the collection): the look wanted first, then the Chinese, Japanese and Korean
/// fonts that also hold Latin, then the symbols.
fn candidates(look: Look) -> Vec<(&'static str, usize)> {
    let cjk: [(&str, usize); 11] = [
        ("msyh.ttc", 0),
        ("simsun.ttc", 0),
        ("simhei.ttf", 0),
        ("msjh.ttc", 0),
        ("mingliu.ttc", 0),
        ("msgothic.ttc", 0),
        ("YuGothR.ttc", 0),
        ("malgun.ttf", 0),
        ("batang.ttc", 0),
        ("arialuni.ttf", 0),
        ("seguisym.ttf", 0),
    ];
    let first: &[(&str, usize)] = match look {
        Look::Sans => &[("arial.ttf", 0), ("segoeui.ttf", 0), ("LiberationSans-Regular.ttf", 0), ("DejaVuSans.ttf", 0)],
        Look::Serif => &[("times.ttf", 0), ("LiberationSerif-Regular.ttf", 0), ("DejaVuSerif.ttf", 0)],
        Look::Mono => &[("cour.ttf", 0), ("consola.ttf", 0), ("LiberationMono-Regular.ttf", 0), ("DejaVuSansMono.ttf", 0)],
    };
    first.iter().chain(cjk.iter()).copied().collect()
}

/// May a font with this `fsType` be embedded (and subset)? Bit 1 (0x0002): restricted licence, which refuses whatever other bits are
/// set (the specification wants bits 1 to 3 exclusive; a font that sets several is read the strict way); bit 8 (0x0100): no
/// subsetting; bit 9 (0x0200): only bitmaps may be embedded. Preview-and-print (0x0004) and editable (0x0008) embedding are
/// allowed. A font with no `OS/2` table has no `fsType` (`None`), which the OpenType specification takes as 0: installable.
fn embedding_allowed(fs_type: Option<u16>) -> std::result::Result<(), &'static str> {
    let Some(fs) = fs_type else { return Ok(()) };
    if fs & 0x0002 != 0 {
        Err("its licence does not allow embedding")
    } else if fs & 0x0100 != 0 {
        Err("its licence does not allow subsetting")
    } else if fs & 0x0200 != 0 {
        Err("its licence allows only bitmaps to be embedded")
    } else {
        Ok(())
    }
}

/// Characters as `U+20000, U+1F600` (the first few, and how many more).
fn code_points(chars: &[char]) -> String {
    let mut list: Vec<String> = chars.iter().take(8).map(|&c| format!("U+{:04X}", c as u32)).collect();
    if chars.len() > 8 {
        list.push(format!("and {} more", chars.len() - 8));
    }
    list.join(", ")
}

/// The system font to put `chars` into a file with: the first of the candidates that is a TrueType font whose licence allows
/// embedding and that has a glyph for every character. When there is none, the error says which characters no candidate has (or,
/// when each is in some font but no font has all of them, how near the best one came).
pub(crate) fn choose(chars: &BTreeSet<char>, look: Look) -> Result<Arc<TtFont>> {
    let mut refused: Option<String> = None;
    let mut seen_cff = false;
    // The characters that every candidate looked at lacks, and the candidate that lacked the fewest.
    let mut nobody: Option<Vec<char>> = None;
    let mut closest: Option<(&str, Vec<char>)> = None;
    for (file, face) in candidates(look) {
        let Some(font) = sysfont::named(file, face) else {
            seen_cff |= sysfont::is_cff_file(file);
            continue;
        };
        if let Err(reason) = embedding_allowed(font.fs_type()) {
            refused.get_or_insert_with(|| format!("the system font {file} is not used: {reason}"));
            continue;
        }
        let Some(cmap) = font.cmap.as_ref() else { continue };
        let missing: Vec<char> = chars.iter().copied().filter(|&c| cmap.unicode(c as u32).is_none_or(|g| g == 0)).collect();
        if missing.is_empty() {
            return Ok(font);
        }
        nobody = Some(match nobody {
            Some(so_far) => so_far.into_iter().filter(|c| missing.contains(c)).collect(),
            None => missing.clone(),
        });
        if closest.as_ref().is_none_or(|(_, least)| missing.len() < least.len()) {
            closest = Some((file, missing));
        }
    }
    Err(Error::Invalid(match (nobody, closest, refused) {
        (Some(nobody), _, _) if !nobody.is_empty() => format!("no installed font has {}", code_points(&nobody)),
        (_, Some((file, missing)), _) => format!("no installed font has all of these characters (the nearest, {file}, lacks {})", code_points(&missing)),
        (_, _, Some(why)) => format!("no system font can show this text: {why}"),
        _ if seen_cff => "the system has only OpenType fonts with PostScript outlines (CFF), which cannot be embedded yet".to_string(),
        _ => "the system has no TrueType font to embed".to_string(),
    }))
}

/// The system font that looks like a font of a page (`req`, from its name and flags) and has every character of `chars` (5a). A
/// request for a face with Latin letters is turned into a Chinese one (Song for a serif face, Hei for the rest) when a character is
/// Chinese, Japanese or Korean; when the font that fits the request lacks something, any font `choose` finds will do.
pub(crate) fn choose_like(chars: &BTreeSet<char>, req: sysfont::Request) -> Result<Arc<TtFont>> {
    use sysfont::{Cjk, Kind, Script};
    let mut req = req;
    if chars.iter().any(|&c| u32::from(c) >= 0x2E80) && !matches!(req.kind, Kind::Cjk(..)) {
        let serif = matches!(req.kind, Kind::Serif | Kind::Georgia | Kind::Cambria);
        req.kind = Kind::Cjk(Script::Gb1, if serif { Cjk::Song } else { Cjk::Hei });
    }
    let fits = |f: &TtFont| embedding_allowed(f.fs_type()).is_ok() && f.cmap.as_ref().is_some_and(|m| chars.iter().all(|&c| m.unicode(c as u32).is_some_and(|g| g != 0)));
    if let Some(font) = sysfont::load(req)
        && fits(&font)
    {
        return Ok(font);
    }
    choose(chars, match req.kind {
        Kind::Mono | Kind::Consolas => Look::Mono,
        Kind::Serif | Kind::Georgia | Kind::Cambria | Kind::Cjk(_, Cjk::Song) => Look::Serif,
        _ => Look::Sans,
    })
}

/// A font program made, and what the text that uses it needs to know.
pub(crate) struct Embedded {
    pub program: Vec<u8>,
    /// `/BaseFont`: a six-letter subset tag, a plus sign and the font's PostScript name.
    pub name: String,
    pub cid: HashMap<char, u16>,
    /// By character code (= glyph number), in thousandths of an em.
    pub widths: Vec<u16>,
    flags: i64,
    bbox: [i64; 4],
    ascent: i64,
    descent: i64,
    cap_height: i64,
    italic_angle: f64,
    bold: bool,
    /// Ascent and descent in ems (descent below zero), for laying lines out.
    pub asc_em: f64,
    pub desc_em: f64,
}

impl Embedded {
    /// The width of a character in thousandths of an em (a character not in the subset has none: 0).
    pub(crate) fn width(&self, c: char) -> f64 {
        self.cid.get(&c).and_then(|&cid| self.widths.get(usize::from(cid))).map_or(0.0, |&w| f64::from(w))
    }

    /// The string operand for `text` (two bytes a character, in hexadecimal), ready for `Tj`.
    pub(crate) fn encode(&self, text: &str) -> String {
        let mut s = String::with_capacity(text.len() * 4 + 2);
        s.push('<');
        for c in text.chars() {
            let _ = write!(s, "{:04X}", self.cid.get(&c).copied().unwrap_or(0));
        }
        s.push('>');
        s
    }
}

fn be16(d: &[u8], at: usize) -> Result<u16> {
    u16_at(d, at).ok_or_else(damaged)
}

fn damaged() -> Error {
    Error::Invalid("the font file is damaged".to_string())
}

fn put16(out: &mut [u8], at: usize, v: u16) -> Result<()> {
    out.get_mut(at..at + 2).ok_or_else(damaged)?.copy_from_slice(&v.to_be_bytes());
    Ok(())
}

/// A glyph record without its instructions, its components (if it is composite) numbered by `map` (original glyph number to the
/// number in the subset).
fn strip(rec: &[u8], map: &mut dyn FnMut(u32) -> Result<u16>, visits: &mut usize) -> Result<Vec<u8>> {
    if rec.is_empty() {
        return Ok(Vec::new());
    }
    let contours = i16::from_be_bytes(be16(rec, 0)?.to_be_bytes());
    if contours >= 0 {
        let at = 10 + 2 * usize::try_from(contours).map_err(|_| damaged())?;
        // (A record of no contours may stop after its header.)
        let Some(length) = u16_at(rec, at) else {
            return if contours == 0 && rec.len() >= 10 { Ok(rec.to_vec()) } else { Err(damaged()) };
        };
        let body = at + 2 + usize::from(length);
        let mut out = rec.get(..at).ok_or_else(damaged)?.to_vec();
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(rec.get(body..).ok_or_else(damaged)?);
        return Ok(out);
    }
    let mut out = rec.get(..10).ok_or_else(damaged)?.to_vec();
    let mut pos = 10usize;
    loop {
        *visits += 1;
        if *visits > MAX_COMPONENT_VISITS {
            return Err(Error::Limit("a font has too many components".to_string()));
        }
        let start = pos;
        let comp = parse_component(rec, &mut pos).map_err(|_| damaged())?;
        let child = map(u32::try_from(comp.child).map_err(|_| damaged())?)?;
        let mut piece = rec.get(start..pos).ok_or_else(damaged)?.to_vec();
        put16(&mut piece, 0, comp.flags & !0x0100)?;
        put16(&mut piece, 2, child)?;
        out.extend_from_slice(&piece);
        if comp.flags & 0x0020 == 0 {
            // (What follows the last component, the instructions, is left out.)
            return Ok(out);
        }
    }
}

fn checksum(d: &[u8]) -> u32 {
    d.chunks(4).fold(0u32, |sum, w| {
        let mut word = [0u8; 4];
        word.get_mut(..w.len()).unwrap_or_default().copy_from_slice(w);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// The tables as an sfnt file: sorted by tag, each padded to four bytes, with their checksums and the font's.
fn assemble(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Result<Vec<u8>> {
    tables.sort_by_key(|t| t.0);
    let n = tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 1, 0, 0]);
    out.extend_from_slice(&u16::try_from(n).map_err(|_| damaged())?.to_be_bytes());
    let mut entry_selector = 0u16;
    while 1usize << (entry_selector + 1) <= n {
        entry_selector += 1;
    }
    let search_range = 16u16 << entry_selector;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&(u16::try_from(n * 16).map_err(|_| damaged())?.saturating_sub(search_range)).to_be_bytes());
    let mut offset = 12 + 16 * n;
    let mut head_at = None;
    for (tag, data) in &tables {
        if tag == b"head" {
            head_at = Some(offset);
        }
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).map_err(|_| damaged())?.to_be_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).map_err(|_| damaged())?.to_be_bytes());
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(4) * 4, 0);
        if out.len() > MAX_FONT_BYTES {
            return Err(Error::Limit("the font program is too big".to_string()));
        }
    }
    // checkSumAdjustment of `head` (it was zero while the sums were made).
    let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    let at = head_at.ok_or_else(damaged)? + 8;
    out.get_mut(at..at + 4).ok_or_else(damaged)?.copy_from_slice(&adjustment.to_be_bytes());
    Ok(out)
}

/// The PostScript name of the font (`name` table, name id 6), reduced to letters, digits and hyphens.
fn postscript_name(font: &TtFont) -> String {
    let clean = |s: String| -> String { s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect() };
    let found = (|| {
        let t = font.raw_table(b"name")?;
        let (count, strings) = (usize::from(u16_at(&t, 2)?), usize::from(u16_at(&t, 4)?));
        for i in 0..count.min(512) {
            let r = 6 + 12 * i;
            if u16_at(&t, r + 6)? != 6 {
                continue;
            }
            let (len, off) = (usize::from(u16_at(&t, r + 8)?), usize::from(u16_at(&t, r + 10)?));
            let raw = t.get(strings + off..strings + off + len)?;
            let text: String = if u16_at(&t, r)? == 3 || u16_at(&t, r)? == 0 {
                raw.as_chunks::<2>().0.iter().map(|p| char::from_u32(u32::from(u16::from_be_bytes(*p))).unwrap_or('?')).collect()
            } else {
                raw.iter().map(|&b| char::from(b)).collect()
            };
            let name = clean(text);
            if !name.is_empty() {
                return Some(name);
            }
        }
        None
    })();
    found.unwrap_or_else(|| "Font".to_string())
}

/// A six-letter tag for the subset, the same for the same font and characters (9.6.4: the tag makes the name unique).
fn subset_tag(name: &str, chars: &BTreeSet<char>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes().chain(chars.iter().flat_map(|c| (*c as u32).to_le_bytes())) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    (0..6).map(|_| {
        let letter = char::from(b'A' + u8::try_from(h % 26).unwrap_or(0));
        h /= 26;
        letter
    }).collect()
}

/// Build the subset of `font` for `chars`. Every character must have a glyph (see [`choose`]).
pub(crate) fn embed(font: &TtFont, chars: &BTreeSet<char>, work: &Work) -> Result<Embedded> {
    if chars.len() + 1 > MAX_GLYPHS {
        return Err(Error::Limit(format!("the text uses more than {MAX_GLYPHS} different characters")));
    }
    embedding_allowed(font.fs_type()).map_err(|why| Error::Invalid(format!("the font is not embedded: {why}")))?;
    let cmap = font.cmap.as_ref().ok_or_else(|| Error::Invalid("the font has no character map".to_string()))?;
    let upem = font.units_per_em();
    let per_mille = |units: f64| -> f64 { units * 1000.0 / upem };

    // The glyphs of the subset: 0 is `.notdef`, then one for each character, then the components they pull in.
    let mut slots: Vec<u32> = vec![0];
    let mut cid: HashMap<char, u16> = HashMap::new();
    for &c in chars {
        let g = cmap.unicode(c as u32).filter(|&g| g != 0).ok_or_else(|| Error::Invalid(format!("the font has no glyph for U+{:04X}", c as u32)))?;
        cid.insert(c, u16::try_from(slots.len()).map_err(|_| damaged())?);
        slots.push(g);
    }
    let mut first: HashMap<u32, usize> = HashMap::new();
    for (i, &g) in slots.iter().enumerate() {
        first.entry(g).or_insert(i);
    }
    let mut records: Vec<Vec<u8>> = Vec::new();
    let mut visits = 0usize;
    let mut total = 0usize;
    let mut i = 0usize;
    while i < slots.len() {
        if !work.charge(GLYPH_COST) {
            return Err(Error::Limit("the font took too much work".to_string()));
        }
        let original = *slots.get(i).ok_or_else(damaged)?;
        let rec = font.glyph_record(original).ok_or_else(damaged)?;
        let record = strip(
            &rec,
            &mut |child| {
                if let Some(&at) = first.get(&child) {
                    return u16::try_from(at).map_err(|_| damaged());
                }
                if slots.len() >= MAX_GLYPHS {
                    return Err(Error::Limit(format!("the font needs more than {MAX_GLYPHS} glyphs for this text")));
                }
                slots.push(child);
                first.insert(child, slots.len() - 1);
                u16::try_from(slots.len() - 1).map_err(|_| damaged())
            },
            &mut visits,
        )?;
        total += record.len().div_ceil(4) * 4;
        if total > MAX_FONT_BYTES {
            return Err(Error::Limit("the font program is too big".to_string()));
        }
        records.push(record);
        i += 1;
    }
    let n = slots.len();

    // glyf, loca (long), hmtx.
    let mut glyf: Vec<u8> = Vec::with_capacity(total);
    let mut loca: Vec<u8> = Vec::with_capacity((n + 1) * 4);
    let mut hmtx: Vec<u8> = Vec::with_capacity(n * 4);
    let mut widths: Vec<u16> = Vec::with_capacity(n);
    for (rec, &original) in records.iter().zip(&slots) {
        loca.extend_from_slice(&u32::try_from(glyf.len()).map_err(|_| damaged())?.to_be_bytes());
        glyf.extend_from_slice(rec);
        glyf.resize(glyf.len().div_ceil(4) * 4, 0);
        let (advance, lsb) = font.metrics_units(original).unwrap_or((0, 0));
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&lsb.to_be_bytes());
        widths.push(per_mille(f64::from(advance)).round().clamp(0.0, 65535.0) as u16);
    }
    loca.extend_from_slice(&u32::try_from(glyf.len()).map_err(|_| damaged())?.to_be_bytes());

    // head, hhea, maxp from the font's own, with the counts and the format of loca changed.
    let mut head = font.raw_table(b"head").filter(|t| t.len() >= 54).ok_or_else(damaged)?;
    if head.get(12..16) != Some(&[0x5F, 0x0F, 0x3C, 0xF5]) {
        return Err(damaged());
    }
    head.get_mut(8..12).ok_or_else(damaged)?.fill(0);
    put16(&mut head, 50, 1)?;
    let mut hhea = font.raw_table(b"hhea").filter(|t| t.len() >= 36).ok_or_else(damaged)?;
    put16(&mut hhea, 34, u16::try_from(n).map_err(|_| damaged())?)?;
    let mut maxp = font.raw_table(b"maxp").filter(|t| t.len() >= 32).ok_or_else(damaged)?;
    put16(&mut maxp, 4, u16::try_from(n).map_err(|_| damaged())?)?;

    // post (format 3: no names), name (none), cmap (an empty format 4 table).
    let old_post = font.raw_table(b"post").unwrap_or_default();
    let mut post = vec![0u8; 32];
    post.get_mut(..4).unwrap_or_default().copy_from_slice(&[0, 3, 0, 0]);
    if old_post.len() >= 16 {
        post.get_mut(4..16).unwrap_or_default().copy_from_slice(old_post.get(4..16).unwrap_or_default());
    }
    let italic_angle = old_post.get(4..8).and_then(|b| b.try_into().ok()).map_or(0.0, |b: [u8; 4]| f64::from(i32::from_be_bytes(b)) / 65536.0);
    let fixed_pitch = old_post.get(12..16).is_some_and(|b| b.iter().any(|&x| x != 0));
    let name_table = vec![0, 0, 0, 0, 0, 6];
    let cmap_table: Vec<u8> = vec![0, 0, 0, 1, 0, 3, 0, 1, 0, 0, 0, 12, 0, 4, 0, 24, 0, 0, 0, 2, 0, 2, 0, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 1, 0, 0];

    let (asc_units, desc_units) = {
        let (a, d) = font.ascender_descender();
        if a == 0 && d == 0 { (upem * 0.8, -upem * 0.2) } else { (f64::from(a), f64::from(d)) }
    };
    let i16_at = |d: &[u8], at: usize| u16_at(d, at).map(|v| f64::from(v as i16)).unwrap_or(0.0);
    let bbox = [per_mille(i16_at(&head, 36)), per_mille(i16_at(&head, 38)), per_mille(i16_at(&head, 40)), per_mille(i16_at(&head, 42))].map(|v| v.round() as i64);
    let os2 = font.raw_table(b"OS/2").unwrap_or_default();
    let cap_height = if os2.len() >= 90 && u16_at(&os2, 0).is_some_and(|v| v >= 2) { per_mille(i16_at(&os2, 88)).round() as i64 } else { 0 };
    let ascent = per_mille(asc_units).round() as i64;
    let descent = per_mille(desc_units).round() as i64;
    let bold = u16_at(&head, 44).is_some_and(|m| m & 1 != 0);
    let italic = italic_angle != 0.0 || u16_at(&head, 44).is_some_and(|m| m & 2 != 0);

    let program = assemble(vec![
        (*b"cmap", cmap_table),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"maxp", maxp),
        (*b"name", name_table),
        (*b"post", post),
    ])?;
    let ps = postscript_name(font);
    Ok(Embedded {
        program,
        name: format!("{}+{}", subset_tag(&ps, chars), ps),
        cid,
        widths,
        flags: 32 | i64::from(fixed_pitch) | if italic { 64 } else { 0 },
        bbox,
        ascent,
        descent,
        cap_height: if cap_height > 0 { cap_height } else { ascent * 7 / 10 },
        italic_angle,
        bold,
        asc_em: asc_units / upem,
        desc_em: desc_units / upem,
    })
}

/// The `/ToUnicode` CMap (9.10.3): every character used, by its character code.
fn to_unicode(cid: &HashMap<char, u16>) -> String {
    let mut entries: Vec<(u16, char)> = cid.iter().map(|(&c, &id)| (id, c)).collect();
    entries.sort_unstable();
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for block in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", block.len());
        for &(id, c) in block {
            let mut units = [0u16; 2];
            let hex: String = c.encode_utf16(&mut units).iter().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(s, "<{id:04X}> <{hex}>");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// Put the font's objects (program, descriptor, CID font, `ToUnicode`, Type 0 font) in the edit and give the Type 0 font.
pub(super) fn add_objects(ctx: &mut Ctx<'_>, f: &Embedded) -> Result<ObjRef> {
    add_objects_in(ctx, f, false)
}

/// [`add_objects`], with the font for vertical writing (`Identity-V`, the default vertical metrics, 9.7.4.3) when `vertical`.
pub(super) fn add_objects_in(ctx: &mut Ctx<'_>, f: &Embedded, vertical: bool) -> Result<ObjRef> {
    let flate = |dict: &mut Dict, data: &[u8]| {
        dict.set("Filter", Object::from("FlateDecode"));
        compress_to_vec_zlib(data, 6)
    };
    let mut file_dict = Dict::new();
    file_dict.set("Length1", Object::Integer(i64::try_from(f.program.len()).unwrap_or(0)));
    let data = flate(&mut file_dict, &f.program);
    let file_ref = ctx.alloc()?;
    ctx.objects.push((file_ref.num, Object::Stream(Stream { dict: file_dict, data })));

    let mut desc = Dict::new();
    desc.set("Type", Object::from("FontDescriptor"));
    desc.set("FontName", Object::from(f.name.as_str()));
    desc.set("Flags", Object::Integer(f.flags));
    desc.set("FontBBox", Object::Array(f.bbox.iter().map(|&v| Object::Integer(v)).collect()));
    desc.set("ItalicAngle", Object::Real(f.italic_angle));
    desc.set("Ascent", Object::Integer(f.ascent));
    desc.set("Descent", Object::Integer(f.descent));
    desc.set("CapHeight", Object::Integer(f.cap_height));
    desc.set("StemV", Object::Integer(if f.bold { 140 } else { 80 }));
    desc.set("FontFile2", Object::Ref(file_ref));
    let desc_ref = ctx.alloc()?;
    ctx.objects.push((desc_ref.num, Object::Dict(desc)));

    let mut cid_font = Dict::new();
    cid_font.set("Type", Object::from("Font"));
    cid_font.set("Subtype", Object::from("CIDFontType2"));
    cid_font.set("BaseFont", Object::from(f.name.as_str()));
    let mut info = Dict::new();
    info.set("Registry", Object::String(PdfString::literal(b"Adobe".to_vec())));
    info.set("Ordering", Object::String(PdfString::literal(b"Identity".to_vec())));
    info.set("Supplement", Object::Integer(0));
    cid_font.set("CIDSystemInfo", Object::Dict(info));
    cid_font.set("FontDescriptor", Object::Ref(desc_ref));
    cid_font.set("DW", Object::Integer(1000));
    cid_font.set("W", Object::Array(vec![Object::Integer(0), Object::Array(f.widths.iter().map(|&w| Object::Integer(i64::from(w))).collect())]));
    cid_font.set("CIDToGIDMap", Object::from("Identity"));
    if vertical {
        cid_font.set("DW2", Object::Array(vec![Object::Integer(880), Object::Integer(-1000)]));
    }
    let cid_ref = ctx.alloc()?;
    ctx.objects.push((cid_ref.num, Object::Dict(cid_font)));

    let mut uni_dict = Dict::new();
    let data = flate(&mut uni_dict, to_unicode(&f.cid).as_bytes());
    let uni_ref = ctx.alloc()?;
    ctx.objects.push((uni_ref.num, Object::Stream(Stream { dict: uni_dict, data })));

    let mut font = Dict::new();
    font.set("Type", Object::from("Font"));
    font.set("Subtype", Object::from("Type0"));
    font.set("BaseFont", Object::from(f.name.as_str()));
    font.set("Encoding", Object::from(if vertical { "Identity-V" } else { "Identity-H" }));
    font.set("DescendantFonts", Object::Array(vec![Object::Ref(cid_ref)]));
    font.set("ToUnicode", Object::Ref(uni_ref));
    let font_ref = ctx.alloc()?;
    ctx.objects.push((font_ref.num, Object::Dict(font)));
    Ok(font_ref)
}

/// For each character of a font put in a page, the bytes that show it and its advance in ems.
pub(super) type Codes = Vec<(char, Vec<u8>, f64)>;

/// A Type 3 font (9.6.5) that has the characters `chars`, their outlines taken from `font`, made for a page whose own font is a Type 3
/// one with the font matrix `m` (5a: text that stands next to glyphs of such a font is drawn the same way up, at the same size, with
/// the same text matrix). Gives the font object and, for each character, the byte that shows it and its advance in ems.
pub(super) fn add_type3(ctx: &mut Ctx<'_>, font: &TtFont, chars: &BTreeSet<char>, m: [f64; 6]) -> Result<(ObjRef, Codes)> {
    embedding_allowed(font.fs_type()).map_err(|why| Error::Invalid(format!("the font is not embedded: {why}")))?;
    if chars.len() > 255 {
        return Err(Error::Limit("a Type 3 font takes at most 255 different characters".to_string()));
    }
    let cmap = font.cmap.as_ref().ok_or_else(|| Error::Invalid("the font has no character map".to_string()))?;
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1.0e-12 || m[0].abs() < 1.0e-12 {
        return Err(Error::Invalid("the Type 3 font has no usable font matrix".to_string()));
    }
    // Ems (the outlines, y up) onto glyph space: the inverse of the font matrix.
    let (inv, e) = ([m[3] / det, -m[1] / det, -m[2] / det, m[0] / det], font.em_matrix());
    let to_glyph = [e[0] * inv[0] + e[1] * inv[2], e[0] * inv[1] + e[1] * inv[3], e[2] * inv[0] + e[3] * inv[2], e[2] * inv[1] + e[3] * inv[3], 0.0, 0.0];
    let (mut procs, mut widths, mut codes, mut names) = (Dict::new(), Vec::new(), Vec::new(), Vec::new());
    let mut cid: HashMap<char, u16> = HashMap::new();
    let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for (i, &c) in chars.iter().enumerate() {
        if !ctx.work.charge(GLYPH_COST) {
            return Err(Error::Limit("the font took too much work".to_string()));
        }
        let gid = cmap.unicode(c as u32).filter(|&g| g != 0).ok_or_else(|| Error::Invalid(format!("the font has no glyph for U+{:04X}", c as u32)))?;
        let adv = f64::from(font.advance(gid).ok_or_else(damaged)?);
        let (proc_text, b) = crate::render::outline::charproc(font, gid, to_glyph, adv / m[0]).ok_or_else(damaged)?;
        bbox = [bbox[0].min(f64::from(b[0])), bbox[1].min(f64::from(b[1])), bbox[2].max(f64::from(b[2])), bbox[3].max(f64::from(b[3]))];
        let mut dict = Dict::new();
        let data = if proc_text.len() > 256 {
            dict.set("Filter", Object::from("FlateDecode"));
            compress_to_vec_zlib(proc_text.as_bytes(), 6)
        } else {
            proc_text.into_bytes()
        };
        ctx.spend(data.len() + 96)?;
        let r = ctx.alloc()?;
        ctx.objects.push((r.num, Object::Stream(Stream { dict, data })));
        let code = u8::try_from(i + 1).map_err(|_| damaged())?;
        let name = format!("g{code}");
        procs.set(name.as_str(), Object::Ref(r));
        names.push(Object::from(name.as_str()));
        widths.push(adv / m[0]);
        codes.push((c, vec![code], adv));
        cid.insert(c, u16::from(code));
    }
    if !bbox.iter().all(|v| v.is_finite()) {
        bbox = [0.0; 4];
    }
    let mut uni_dict = Dict::new();
    uni_dict.set("Filter", Object::from("FlateDecode"));
    let uni_data = compress_to_vec_zlib(to_unicode(&cid).as_bytes(), 6);
    ctx.spend(uni_data.len() + 96)?;
    let uni_ref = ctx.alloc()?;
    ctx.objects.push((uni_ref.num, Object::Stream(Stream { dict: uni_dict, data: uni_data })));
    let mut encoding = Dict::new();
    encoding.set("Type", Object::from("Encoding"));
    encoding.set("Differences", Object::Array(std::iter::once(Object::Integer(1)).chain(names).collect()));
    let mut f = Dict::new();
    f.set("Type", Object::from("Font"));
    f.set("Subtype", Object::from("Type3"));
    f.set("FontBBox", crate::layout::reals(&bbox));
    f.set("FontMatrix", crate::layout::reals(&m));
    f.set("CharProcs", Object::Dict(procs));
    f.set("Encoding", Object::Dict(encoding));
    f.set("FirstChar", Object::Integer(1));
    f.set("LastChar", Object::Integer(i64::try_from(chars.len()).unwrap_or(0)));
    f.set("Widths", crate::layout::reals(&widths));
    f.set("Resources", Object::Dict(Dict::new()));
    f.set("ToUnicode", Object::Ref(uni_ref));
    ctx.spend(f.approx_size() + 48)?;
    let font_ref = ctx.alloc()?;
    ctx.objects.push((font_ref.num, Object::Dict(f)));
    Ok((font_ref, codes))
}

impl crate::layout::Face for Embedded {
    fn width(&self, c: char) -> f64 {
        Embedded::width(self, c)
    }

    fn ascent_descent(&self) -> (f64, f64) {
        (self.asc_em, self.desc_em)
    }

    fn encode(&self, text: &str) -> String {
        Embedded::encode(self, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subset_draws_the_same_glyphs_and_has_the_same_widths_as_the_font_it_was_made_from() {
        use crate::render::outline::Builder;
        // (An accented letter is usually a composite glyph: its parts are pulled in and renumbered.)
        let chars: BTreeSet<char> = "\u{4E2D}a\u{E9}\u{C4},".chars().collect();
        let Ok(font) = choose(&chars, Look::Sans) else {
            println!("SKIPPED: no font on this machine has these characters");
            return;
        };
        let emb = embed(&font, &chars, &Work::new()).unwrap();
        let sub = TtFont::from_memory(Arc::new(emb.program.clone()), 0).unwrap();
        let shape = |f: &TtFont, gid: u32| {
            let mut b = Builder::new(f.em_matrix());
            f.outline(gid, &mut b).unwrap();
            b.finish().map(|p| {
                let r = p.bounds();
                ([r.left(), r.top(), r.right(), r.bottom()].map(f32::to_bits), p.len())
            })
        };
        for c in &chars {
            let gid = font.cmap.as_ref().unwrap().unicode(*c as u32).unwrap();
            let cid = u32::from(emb.cid[c]);
            assert_eq!(shape(&font, gid), shape(&sub, cid), "the outline of {c}");
            let (original, copy) = (font.advance(gid).unwrap(), sub.advance(cid).unwrap());
            assert!((original - copy).abs() < 1e-6, "the width of {c}: {original} and {copy}");
            assert!((f64::from(copy) * 1000.0 - emb.width(*c)).abs() < 1.0);
        }
        // The font file is small.
        assert!(emb.program.len() < 60_000, "{} bytes", emb.program.len());
    }

    #[test]
    fn licences_are_kept() {
        assert!(embedding_allowed(None).is_ok());
        assert!(embedding_allowed(Some(0)).is_ok());
        assert!(embedding_allowed(Some(0x0004)).is_ok());
        assert!(embedding_allowed(Some(0x0008)).is_ok());
        assert!(embedding_allowed(Some(0x0002)).is_err());
        // Restricted refuses whatever else is set (0x0006 and 0x000A are not licences).
        assert!(embedding_allowed(Some(0x0006)).is_err());
        assert!(embedding_allowed(Some(0x000A)).is_err());
        assert!(embedding_allowed(Some(0x000E)).is_err());
        assert!(embedding_allowed(Some(0x0100)).is_err());
        assert!(embedding_allowed(Some(0x0200)).is_err());
        assert!(embedding_allowed(Some(0x0008 | 0x0100)).is_err());
    }

    #[test]
    fn the_unicode_map_has_every_character_and_the_checksum_adds_up() {
        let cid: HashMap<char, u16> = [('\u{4E2D}', 1), ('a', 2), ('\u{1F600}', 3)].into_iter().collect();
        let map = to_unicode(&cid);
        assert!(map.contains("<0001> <4E2D>") && map.contains("<0002> <0061>") && map.contains("<0003> <D83DDE00>"), "{map}");
        assert!(map.contains("3 beginbfchar"));
        // A font file's checksum, adjustment included, is 0xB1B0AFBA.
        let file = assemble(vec![(*b"head", vec![0; 54]), (*b"maxp", vec![0; 6])]).unwrap();
        assert_eq!(checksum(&file), 0xB1B0_AFBA);
    }

    #[test]
    fn a_simple_glyph_loses_its_instructions_and_a_composite_is_renumbered() {
        // One contour, three instruction bytes, then the rest of the glyph (flags, coordinates).
        let mut simple = vec![0, 1, 0, 0, 0, 0, 0, 10, 0, 10, 0, 2, 0, 3, 1, 2, 3, 0x11, 5, 6];
        let stripped = strip(&simple, &mut |_| Ok(0), &mut 0).unwrap();
        assert_eq!(stripped, vec![0, 1, 0, 0, 0, 0, 0, 10, 0, 10, 0, 2, 0, 0, 0x11, 5, 6]);
        // A composite of two components (words as arguments, then bytes), with instructions after the last one.
        let composite = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x23, 0, 7, 0, 1, 0, 2, 0x01, 0x03, 0, 9, 3, 4, 0, 2, 0xAA, 0xBB];
        let mut seen = Vec::new();
        let out = strip(&composite, &mut |g| { seen.push(g); Ok(u16::try_from(g).unwrap() + 100) }, &mut 0).unwrap();
        assert_eq!(seen, vec![7, 9]);
        assert_eq!(out, vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x23, 0, 107, 0, 1, 0, 2, 0x00, 0x03, 0, 109, 3, 4, 0, 2]);
        simple.truncate(5);
        assert!(strip(&simple, &mut |_| Ok(0), &mut 0).is_err(), "a cut glyph is an error");
    }
}
