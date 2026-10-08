//! Tests of the font code: fonts built here, byte by byte (a TrueType, a CFF and a Type 1 font), drawn by their readers and by
//! pages; and hostile ones (truncated tables, offsets out of range, composite glyphs that include themselves, subroutine loops,
//! charstrings that never end, garbage where the encrypted part should be) that must be errors or nothing, quickly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tiny_skia::Rect;

use super::cff::CffFont;
use super::fonts::GlyphSource;
use super::glyf::TtFont;
use super::outline::Builder;
use super::tests::{WHITE, draw, page_doc, pixel};
use super::type1::T1Font;
use crate::object::{ObjRef, Object};
use crate::text::font::{Font, Warnings};

// --- building fonts ---------------------------------------------------------------------------------------

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn sfnt(version: &[u8; 4], tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut out = version.to_vec();
    out.extend(be16(tables.len() as u16));
    out.extend([0u8; 6]);
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in tables {
        out.extend(*tag);
        out.extend(be32(0));
        out.extend(be32(offset as u32));
        out.extend(be32(data.len() as u32));
        body.extend(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend(body);
    out
}

/// A simple glyph: contours of (x, y, on the curve) points.
fn tt_glyph(contours: &[&[(i16, i16, bool)]]) -> Vec<u8> {
    tt_glyph_ins(contours, &[])
}

/// A simple glyph with instructions.
fn tt_glyph_ins(contours: &[&[(i16, i16, bool)]], ins: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(be16(contours.len() as u16));
    out.extend([0u8; 8]);
    let mut end = 0u16;
    for c in contours {
        end += c.len() as u16;
        out.extend(be16(end - 1));
    }
    out.extend(be16(ins.len() as u16));
    out.extend(ins);
    let points: Vec<_> = contours.iter().flat_map(|c| c.iter()).collect();
    for p in &points {
        out.push(u8::from(p.2));
    }
    let mut prev = 0i16;
    for p in &points {
        out.extend(((p.0 - prev) as u16).to_be_bytes());
        prev = p.0;
    }
    prev = 0;
    for p in &points {
        out.extend(((p.1 - prev) as u16).to_be_bytes());
        prev = p.1;
    }
    out
}

fn tt_rect(x0: i16, y0: i16, x1: i16, y1: i16) -> Vec<u8> {
    tt_glyph(&[&[(x0, y0, true), (x0, y1, true), (x1, y1, true), (x1, y0, true)]])
}

/// A composite glyph: (glyph, dx, dy) components.
fn tt_composite(parts: &[(u16, i16, i16)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((-1i16).to_be_bytes());
    out.extend([0u8; 8]);
    for (i, (gid, dx, dy)) in parts.iter().enumerate() {
        let more = if i + 1 < parts.len() { 0x20 } else { 0 };
        out.extend(be16(0x0003 | more));
        out.extend(be16(*gid));
        out.extend(dx.to_be_bytes());
        out.extend(dy.to_be_bytes());
    }
    out
}

/// A cmap with one subtable of format 4 (one segment per code).
fn cmap_format4(platform: u16, encoding: u16, map: &[(u16, u16)]) -> Vec<u8> {
    let mut sub = Vec::new();
    let segs = map.len() + 1;
    sub.extend(be16(4));
    sub.extend(be16(0));
    sub.extend(be16(0));
    sub.extend(be16((segs * 2) as u16));
    sub.extend([0u8; 6]);
    for (code, _) in map {
        sub.extend(be16(*code));
    }
    sub.extend(be16(0xFFFF));
    sub.extend(be16(0));
    for (code, _) in map {
        sub.extend(be16(*code));
    }
    sub.extend(be16(0xFFFF));
    for (code, gid) in map {
        sub.extend(be16(gid.wrapping_sub(*code)));
    }
    sub.extend(be16(1));
    for _ in 0..segs {
        sub.extend(be16(0));
    }
    let len = sub.len() as u16;
    sub[2..4].copy_from_slice(&be16(len));
    let mut out = Vec::new();
    out.extend(be16(0));
    out.extend(be16(1));
    out.extend(be16(platform));
    out.extend(be16(encoding));
    out.extend(be32(12));
    out.extend(sub);
    out
}

struct Tt {
    glyphs: Vec<Vec<u8>>,
    cmap: Vec<u8>,
    long_loca: bool,
}

impl Tt {
    fn build(&self) -> Vec<u8> {
        let mut glyf = Vec::new();
        let mut offsets = vec![0u32];
        for g in &self.glyphs {
            glyf.extend(g);
            while glyf.len() % 4 != 0 {
                glyf.push(0);
            }
            offsets.push(glyf.len() as u32);
        }
        let loca: Vec<u8> = if self.long_loca { offsets.iter().flat_map(|o| be32(*o)).collect() } else { offsets.iter().flat_map(|o| be16((*o / 2) as u16)).collect() };
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&be16(1000));
        head[50..52].copy_from_slice(&be16(u16::from(self.long_loca)));
        let mut maxp = vec![0u8; 6];
        maxp[4..6].copy_from_slice(&be16(self.glyphs.len() as u16));
        let mut hhea = vec![0u8; 36];
        hhea[34..36].copy_from_slice(&be16(1));
        let mut hmtx = Vec::new();
        hmtx.extend(be16(1000));
        hmtx.extend(be16(0));
        sfnt(&[0, 1, 0, 0], &[(b"cmap", self.cmap.clone()), (b"glyf", glyf), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx), (b"loca", loca), (b"maxp", maxp)])
    }
}

/// The font of most tests: glyph 1 is a square of 800 units, glyph 2 a tall bar.
fn test_truetype() -> Vec<u8> {
    Tt {
        glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800), tt_rect(100, 0, 300, 900)],
        cmap: cmap_format4(3, 1, &[(65, 1), (66, 2)]),
        long_loca: false,
    }
    .build()
}

fn load_tt(data: Vec<u8>) -> Result<TtFont, &'static str> {
    TtFont::from_memory(Arc::new(data), 0)
}

fn tt_bounds(tt: &TtFont, gid: u32) -> Result<Option<Rect>, &'static str> {
    let mut b = Builder::new(tt.em_matrix());
    tt.outline(gid, &mut b)?;
    Ok(b.finish().map(|p| p.bounds()))
}

// --- CFF -----------------------------------------------------------------------------------------------------

fn cff_num(v: i32) -> Vec<u8> {
    if (-107..=107).contains(&v) { vec![(v + 139) as u8] } else { vec![28, (v >> 8) as u8, v as u8] }
}

fn cs(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn nums(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|&v| cff_num(v)).collect()
}

fn cff_index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(be16(items.len() as u16));
    if items.is_empty() {
        return out;
    }
    out.push(4);
    let mut offset = 1u32;
    out.extend(be32(offset));
    for it in items {
        offset += it.len() as u32;
        out.extend(be32(offset));
    }
    for it in items {
        out.extend(it);
    }
    out
}

fn dict_int5(v: i32) -> Vec<u8> {
    let mut out = vec![29];
    out.extend(v.to_be_bytes());
    out
}

/// A name-keyed CFF font: glyph 0 is .notdef, the others have the names given (custom strings).
fn cff_font(names: &[&str], charstrings: &[Vec<u8>], gsubrs: &[Vec<u8>], lsubrs: &[Vec<u8>]) -> Vec<u8> {
    let header = vec![1, 0, 4, 4];
    let name_index = cff_index(&[b"T".to_vec()]);
    let strings: Vec<Vec<u8>> = names.iter().map(|n| n.as_bytes().to_vec()).collect();
    let string_index = cff_index(&strings);
    let gsubr_index = cff_index(gsubrs);
    // Top DICT: charset, CharStrings, Private (all with 5 byte numbers, so its size is known).
    let top_size = 6 + 6 + 11;
    let top_index_len = 2 + 1 + 8 + top_size;
    let after_top = header.len() + name_index.len() + top_index_len + string_index.len() + gsubr_index.len();
    let mut charset = vec![0u8];
    for i in 0..names.len() {
        charset.extend(be16((391 + i) as u16));
    }
    let charset_off = after_top;
    let charstrings_off = charset_off + charset.len();
    let charstrings_index = cff_index(charstrings);
    let private_off = charstrings_off + charstrings_index.len();
    let mut private = Vec::new();
    private.extend(dict_int5(0));
    private.push(20);
    private.extend(dict_int5(0));
    private.push(21);
    let with_subrs = !lsubrs.is_empty();
    if with_subrs {
        private.extend(dict_int5((private.len() + 6) as i32));
        private.push(19);
    }
    let mut top = Vec::new();
    top.extend(dict_int5(charset_off as i32));
    top.push(15);
    top.extend(dict_int5(charstrings_off as i32));
    top.push(17);
    top.extend(dict_int5(private.len() as i32));
    top.extend(dict_int5(private_off as i32));
    top.push(18);
    assert_eq!(top.len(), top_size);
    let mut out = header;
    out.extend(name_index);
    out.extend(cff_index(&[top]));
    out.extend(string_index);
    out.extend(gsubr_index);
    out.extend(charset);
    out.extend(charstrings_index);
    out.extend(private);
    if with_subrs {
        out.extend(cff_index(lsubrs));
    }
    out
}

fn cff_bounds(font: &CffFont, gid: u32) -> Result<Option<Rect>, &'static str> {
    let mut b = font.builder(gid);
    font.outline(gid, &mut b)?;
    Ok(b.finish().map(|p| p.bounds()))
}

fn load_cff(data: Vec<u8>) -> Result<CffFont, &'static str> {
    let len = data.len();
    CffFont::parse(Arc::new(data), 0, len, None)
}

/// A square of `side` units from the origin with rmoveto and rlineto.
fn square(side: i32) -> Vec<u8> {
    cs(&[&nums(&[0, 0]), &[21], &nums(&[side, 0, 0, side, -side]), &[5], &[14]])
}

// --- Type 1 --------------------------------------------------------------------------------------------------

fn t1_num(v: i32) -> Vec<u8> {
    if (-107..=107).contains(&v) {
        vec![(v + 139) as u8]
    } else {
        let mut out = vec![255];
        out.extend(v.to_be_bytes());
        out
    }
}

fn t1_nums(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|&v| t1_num(v)).collect()
}

fn encrypt(data: &[u8], key: u16, prefix: [u8; 4]) -> Vec<u8> {
    let mut r = key;
    let mut out = Vec::new();
    for &p in prefix.iter().chain(data) {
        let c = p ^ (r >> 8) as u8;
        r = u16::from(c).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
        out.push(c);
    }
    out
}

/// A Type 1 program: `glyphs` are (name, charstring) and `subrs` the subroutines.
fn type1(glyphs: &[(&str, Vec<u8>)], subrs: &[Vec<u8>], encoding: &[(u8, &str)]) -> Vec<u8> {
    let mut clear = String::from("%!PS-AdobeFont-1.0: Test 1.0\n/FontName /Test def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n");
    for (code, name) in encoding {
        clear.push_str(&format!("dup {code} /{name} put\n"));
    }
    clear.push_str("readonly def\ncurrentdict end\ncurrentfile eexec\n");
    let mut private: Vec<u8> = b"dup /Private 8 dict dup begin\n/RD {string currentfile exch readstring pop} executeonly def\n/ND {noaccess def} executeonly def\n/NP {noaccess put} executeonly def\n/lenIV 4 def\n".to_vec();
    private.extend(format!("/Subrs {} array\n", subrs.len()).bytes());
    for (i, s) in subrs.iter().enumerate() {
        let enc = encrypt(s, 4330, [1, 2, 3, 4]);
        private.extend(format!("dup {i} {} RD ", enc.len()).bytes());
        private.extend(enc);
        private.extend(b" NP\n");
    }
    private.extend(b"ND\n");
    private.extend(format!("2 index /CharStrings {} dict dup begin\n", glyphs.len()).bytes());
    for (name, c) in glyphs {
        let enc = encrypt(c, 4330, [1, 2, 3, 4]);
        private.extend(format!("/{name} {} RD ", enc.len()).bytes());
        private.extend(enc);
        private.extend(b" ND\n");
    }
    private.extend(b"end end readonly put noaccess put dup /FontName get exch definefont pop mark currentfile closefile\n");
    let mut out = clear.into_bytes();
    out.extend(encrypt(&private, 55665, [0xA5, 0x37, 0x91, 0xC2]));
    out.extend(b"\n0000000000000000000000000000000000000000000000000000000000000000\ncleartomark\n");
    out
}

/// hsbw, a square with rlineto and closepath, endchar.
fn t1_square(side: i32) -> Vec<u8> {
    cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 0]), &[21], &t1_num(side), &[6], &t1_num(side), &[7], &t1_num(-side), &[6], &[9], &[14]])
}

fn load_t1(data: &[u8]) -> Result<T1Font, &'static str> {
    T1Font::parse(data)
}

fn t1_bounds(font: &T1Font, gid: u32) -> Result<Option<Rect>, &'static str> {
    let mut b = font.builder();
    font.outline(gid, &mut b)?;
    Ok(b.finish().map(|p| p.bounds()))
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.002
}

fn assert_bounds(r: Rect, l: f32, t: f32, rr: f32, b: f32) {
    assert!(close(r.left(), l) && close(r.top(), t) && close(r.right(), rr) && close(r.bottom(), b), "{r:?} is not ({l}, {t}, {rr}, {b})");
}

// --- the readers ---------------------------------------------------------------------------------------------

#[test]
fn truetype_simple_composite_and_curves() {
    let tt = load_tt(test_truetype()).expect("loads");
    assert_bounds(tt_bounds(&tt, 1).unwrap().unwrap(), 0.0, 0.0, 0.8, 0.8);
    assert_eq!(tt.advance(1), Some(1.0));
    // A composite: the square moved by (100, 50), then the bar by (0, 200).
    let font = Tt {
        glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800), tt_rect(100, 0, 300, 900), tt_composite(&[(1, 100, 50), (2, 0, 200)])],
        cmap: cmap_format4(3, 1, &[(65, 3)]),
        long_loca: true,
    }
    .build();
    let tt = load_tt(font).expect("loads");
    assert_bounds(tt_bounds(&tt, 3).unwrap().unwrap(), 0.1, 0.05, 0.9, 1.1);
    // Off-curve points: a contour of only off-curve points still closes into a curve (a diamond of 4 control points).
    let round = tt_glyph(&[&[(0, 500, false), (500, 1000, false), (1000, 500, false), (500, 0, false)]]);
    let tt = load_tt(Tt { glyphs: vec![tt_rect(0, 0, 0, 0), round], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build()).expect("loads");
    let b = tt_bounds(&tt, 1).unwrap().unwrap();
    assert!(b.left() >= -0.001 && b.right() <= 1.001 && b.right() > 0.7, "{b:?}");
    // The cmap of the test font.
    assert_eq!(load_tt(test_truetype()).unwrap().cmap.as_ref().unwrap().unicode(66), Some(2));
    assert_eq!(load_tt(test_truetype()).unwrap().cmap.as_ref().unwrap().unicode(67), None);
}

#[test]
fn cff_outlines_subroutines_hints_and_curves() {
    // Glyph 1: a square. Glyph 2: a square through a local subroutine (bias 107: number -107 is subr 0) and a global one.
    let local = cs(&[&nums(&[300, 0, 0, 300, -300]), &[5], &[11]]);
    let global = cs(&[&nums(&[10, 10]), &[21], &[11]]);
    let two = cs(&[&nums(&[-107]), &[29], &nums(&[-107]), &[10], &[14]]);
    // Glyph 3: hstem, vstem, hintmask (one stem each, so one mask byte), then a curve.
    let three = cs(&[&nums(&[0, 100, 0, 100]), &[1, 3], &[19, 0xC0], &nums(&[0, 0]), &[21], &nums(&[100, 0, 100, 100, 0, 100, -200]), &[8], &[14]]);
    let font = cff_font(&["one", "two", "three"], &[cs(&[&[14]]), square(500), two, three], &[global], &[local]);
    let cff = load_cff(font).expect("loads");
    assert_eq!(cff.gid_of_name("two"), Some(2));
    assert_bounds(cff_bounds(&cff, 1).unwrap().unwrap(), 0.0, 0.0, 0.5, 0.5);
    assert_bounds(cff_bounds(&cff, 2).unwrap().unwrap(), 0.01, 0.01, 0.31, 0.31);
    let b = cff_bounds(&cff, 3).unwrap().unwrap();
    assert!(close(b.left(), 0.0) && b.right() > 0.19, "{b:?}");
}

#[test]
fn cff_seac_places_an_accent_over_a_base() {
    // endchar with adx ady bchar achar: base A (65), accent acute (194), accent moved by (100, 200).
    let a_sid = crate::text::fontprog::standard_encoding_sid(65).unwrap();
    let acute_sid = crate::text::fontprog::standard_encoding_sid(194).unwrap();
    assert_eq!(crate::text::fontprog::standard_encoding_name(65), Some("A"));
    assert_eq!(crate::text::fontprog::standard_encoding_name(194), Some("acute"));
    // The charset lists custom strings; here the glyph names are taken as the standard strings by their SIDs, so build the
    // font by hand with a charset of standard SIDs.
    let charstrings = [cs(&[&[14]]), square(400), square(100), cs(&[&nums(&[100, 200, 65, 194]), &[14]])];
    let mut font = cff_font(&["x", "y", "z"], &charstrings, &[], &[]);
    // Patch the charset (format 0, after the global subr INDEX) to the standard SIDs of A, acute and a custom name.
    let at = font.windows(5).position(|w| w == [0, 0x01, 0x87, 0x01, 0x88]).expect("charset");
    font[at + 1..at + 3].copy_from_slice(&be16(a_sid));
    font[at + 3..at + 5].copy_from_slice(&be16(acute_sid));
    let cff = load_cff(font).expect("loads");
    // The seac glyph (3) is the square of 400 plus the square of 100 at (100, 200).
    let b = cff_bounds(&cff, 3).unwrap().unwrap();
    assert_bounds(b, 0.0, 0.0, 0.4, 0.4);
    let mut b2 = cff.builder(3);
    cff.outline(3, &mut b2).unwrap();
    assert!(b2.finish().unwrap().len() >= 8, "two contours");
}

#[test]
fn type1_outlines_subroutines_flex_and_seac() {
    // A square, a square through a subroutine, a glyph with flex (7 points through the standard Subrs 0, 1, 2).
    let s0 = cs(&[&t1_nums(&[3, 0]), &[12, 16], &[12, 17], &[12, 17], &[12, 33], &[11]]);
    let s1 = cs(&[&t1_nums(&[0, 1]), &[12, 16], &[11]]);
    let s2 = cs(&[&t1_nums(&[0, 2]), &[12, 16], &[11]]);
    let s3 = cs(&[&[11]]);
    let s4 = cs(&[&t1_nums(&[200, 0]), &[5], &[11]]);
    let via_subr = cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 0]), &[21], &t1_num(4), &[10], &t1_nums(&[200]), &[7], &t1_nums(&[-200]), &[6], &[9], &[14]]);
    // flex: 1 callsubr, then 7 x (rmoveto, 2 callsubr), then flexheight x y 0 callsubr.
    let mut flex = cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 0]), &[21], &t1_num(1), &[10]]);
    for (dx, dy) in [(50, 0), (50, 20), (50, 20), (50, 0), (50, -20), (50, -20), (50, 0)] {
        flex.extend(cs(&[&t1_nums(&[dx, dy]), &[21], &t1_num(2), &[10]]));
    }
    flex.extend(cs(&[&t1_nums(&[50, 350, 0]), &t1_num(0), &[10], &t1_nums(&[100]), &[7], &[9], &[14]]));
    let font = type1(
        &[(".notdef", cs(&[&t1_nums(&[0, 500]), &[13], &[14]])), ("A", t1_square(400)), ("B", via_subr), ("C", flex), ("acute", t1_square(100)),
          ("Aacute", cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 100, 200, 65, 194]), &[12, 6]]))],
        &[s0, s1, s2, s3, s4],
        &[(65, "A"), (66, "B")],
    );
    let t1 = load_t1(&font).expect("loads");
    let gid = |n: &str| t1.gid_of_name(n).unwrap();
    assert_eq!(t1.gid_of_code(65), Some(gid("A")));
    assert_bounds(t1_bounds(&t1, gid("A")).unwrap().unwrap(), 0.0, 0.0, 0.4, 0.4);
    assert_bounds(t1_bounds(&t1, gid("B")).unwrap().unwrap(), 0.0, 0.0, 0.2, 0.2);
    // The flex glyph: a line through 350 units across (two curves), then up.
    let b = t1_bounds(&t1, gid("C")).unwrap().unwrap();
    assert!(b.right() > 0.3 && b.right() < 0.4 && b.bottom() > 0.04, "{b:?}");
    // seac: the base A (400) and the accent (100 units) moved by sbx + adx - asb = 0 + 100 - 0, ady = 200: no taller than 0.3.
    let b = t1_bounds(&t1, gid("Aacute")).unwrap().unwrap();
    assert_bounds(b, 0.0, 0.0, 0.4, 0.4);
    // The font in the PFB form, and the encrypted part in hexadecimal, read the same.
    let mut pfb = vec![0x80, 1];
    let split = font.windows(5).position(|w| w == b"eexec").unwrap() + 6;
    pfb.extend(be32(split as u32).iter().rev());
    pfb.extend(&font[..split]);
    let end = font.len();
    pfb.extend([0x80, 2]);
    pfb.extend(((end - split) as u32).to_le_bytes());
    pfb.extend(&font[split..]);
    pfb.extend([0x80, 3]);
    let t1b = load_t1(&pfb).expect("PFB loads");
    assert_bounds(t1_bounds(&t1b, t1b.gid_of_name("A").unwrap()).unwrap().unwrap(), 0.0, 0.0, 0.4, 0.4);
    let mut hex = font[..split].to_vec();
    for byte in &font[split..font.len() - 80] {
        hex.extend(format!("{byte:02x}").bytes());
    }
    hex.extend(b"\n0000000000\ncleartomark\n");
    let t1h = load_t1(&hex).expect("hex loads");
    assert_bounds(t1_bounds(&t1h, t1h.gid_of_name("A").unwrap()).unwrap().unwrap(), 0.0, 0.0, 0.4, 0.4);
}

// --- hostile fonts -----------------------------------------------------------------------------------------

/// All glyphs of a font are tried: errors are fine, a panic or a long time is not.
fn try_all_tt(data: Vec<u8>) {
    let started = Instant::now();
    if let Ok(tt) = load_tt(data) {
        for g in 0..tt.num_glyphs_for_test().min(3000) {
            let _ = tt_bounds(&tt, g as u32);
        }
    }
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
}

#[test]
fn hostile_truetype_tables_and_glyphs() {
    let good = test_truetype();
    // Truncated at every length.
    for len in (0..good.len()).step_by(7) {
        try_all_tt(good[..len].to_vec());
    }
    // loca out of range: offsets past the end of glyf, offsets that run backwards.
    let mut font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800), tt_rect(100, 0, 300, 900)], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: true }.build();
    let loca_at = font.windows(4).position(|w| w == b"loca").unwrap();
    let off = u32::from_be_bytes(font[loca_at + 8..loca_at + 12].try_into().unwrap()) as usize;
    font[off + 4..off + 8].copy_from_slice(&be32(0x7FFF_FFF0));
    font[off + 8..off + 12].copy_from_slice(&be32(4));
    font[off + 12..off + 16].copy_from_slice(&be32(2));
    let tt = load_tt(font.clone()).expect("the tables are there");
    assert!(tt_bounds(&tt, 1).is_err(), "an offset past glyf is an error");
    assert!(tt_bounds(&tt, 2).is_err(), "offsets running backwards are an error");
    assert!(tt_bounds(&tt, 99).is_err());
    try_all_tt(font);
    // A composite that includes itself, two that include each other, a chain deeper than the limit.
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_composite(&[(1, 0, 0)]), tt_composite(&[(3, 0, 0)]), tt_composite(&[(2, 0, 0)])], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build();
    let tt = load_tt(font).unwrap();
    for g in 1..4 {
        assert_eq!(tt_bounds(&tt, g), Err("a composite glyph includes itself"));
    }
    let mut chain = vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 100, 100)];
    for i in 1..20u16 {
        chain.push(tt_composite(&[(i, 1, 1)]));
    }
    let tt = load_tt(Tt { glyphs: chain, cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build()).unwrap();
    assert!(tt_bounds(&tt, 5).is_ok());
    assert_eq!(tt_bounds(&tt, 19), Err("composite glyphs nested too deep"));
    // Too many components: 300 components, then a tree of components that would be a million glyphs.
    let many: Vec<(u16, i16, i16)> = (0..300).map(|_| (1, 1, 1)).collect();
    let tt = load_tt(Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 100, 100), tt_composite(&many)], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build()).unwrap();
    assert_eq!(tt_bounds(&tt, 2), Err("a glyph has too many components"));
    let mut tree = vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 100, 100)];
    for i in 1..7u16 {
        let kids: Vec<(u16, i16, i16)> = (0..10).map(|_| (i, 0, 0)).collect();
        tree.push(tt_composite(&kids));
    }
    let tt = load_tt(Tt { glyphs: tree, cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build()).unwrap();
    let started = Instant::now();
    assert!(tt_bounds(&tt, 6).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    // A glyph that claims 60,000 points and brings a few bytes; end points that run backwards.
    let mut bad = tt_rect(0, 0, 10, 10);
    bad[10..12].copy_from_slice(&be16(59_999));
    let tt = load_tt(Tt { glyphs: vec![tt_rect(0, 0, 0, 0), bad], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build()).unwrap();
    assert!(tt_bounds(&tt, 1).is_err());
    let mut bad = tt_glyph(&[&[(0, 0, true), (0, 9, true), (9, 9, true)], &[(1, 1, true), (1, 2, true), (2, 2, true)]]);
    bad[10..12].copy_from_slice(&be16(5));
    bad[12..14].copy_from_slice(&be16(2));
    let tt = load_tt(Tt { glyphs: vec![tt_rect(0, 0, 0, 0), bad], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: false }.build()).unwrap();
    assert!(tt_bounds(&tt, 1).is_err());
    // More glyphs claimed than loca has entries; a tiny units per em; a head table that is too short.
    let mut font = test_truetype();
    let maxp_at = font.windows(4).position(|w| w == b"maxp").unwrap();
    let off = u32::from_be_bytes(font[maxp_at + 8..maxp_at + 12].try_into().unwrap()) as usize;
    font[off + 4..off + 6].copy_from_slice(&be16(60_000));
    let tt = load_tt(font).unwrap();
    assert!(tt_bounds(&tt, 40_000).is_err());
    let mut font = test_truetype();
    let head_at = font.windows(4).position(|w| w == b"head").unwrap();
    let off = u32::from_be_bytes(font[head_at + 8..head_at + 12].try_into().unwrap()) as usize;
    font[off + 18..off + 20].copy_from_slice(&be16(0));
    assert!(load_tt(font).is_err());
}

#[test]
fn random_damage_to_truetype_cff_and_type1_never_panics() {
    let mut state = 12345u64;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let tt = test_truetype();
    let cff = cff_font(&["one", "two"], &[cs(&[&[14]]), square(500), cs(&[&nums(&[1, 2]), &[21], &nums(&[-107]), &[10], &[14]])], &[cs(&[&[11]])], &[cs(&[&nums(&[5, 5]), &[5], &[11]])]);
    let t1 = type1(&[(".notdef", cs(&[&t1_nums(&[0, 500]), &[13], &[14]])), ("A", t1_square(400))], &[cs(&[&[11]])], &[(65, "A")]);
    let started = Instant::now();
    for round in 0..1500 {
        for (kind, source) in [(0, &tt), (1, &cff), (2, &t1)] {
            let mut d = source.clone();
            for _ in 0..1 + round % 4 {
                let at = next() % d.len();
                d[at] = next() as u8;
            }
            if round % 5 == 0 {
                d.truncate(next() % d.len());
            }
            match kind {
                0 => try_all_tt(d),
                1 => {
                    if let Ok(f) = load_cff(d) {
                        for g in 0..f.num_glyphs().min(50) as u32 {
                            let _ = cff_bounds(&f, g);
                        }
                        let _ = (f.gid_of_name("one"), f.gid_of_unicode(65), f.gid_of_code(65), f.gid_of_cid(3));
                    }
                }
                _ => {
                    if let Ok(f) = load_t1(&d) {
                        for g in 0..4u32 {
                            let _ = t1_bounds(&f, g);
                        }
                        let _ = (f.gid_of_name("A"), f.gid_of_unicode(65), f.gid_of_code(65));
                    }
                }
            }
        }
    }
    assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
}

#[test]
fn hostile_cff_loops_stacks_and_huge_charstrings() {
    let started = Instant::now();
    // A local subroutine that calls itself; one that calls a missing subroutine; deep chains of global subroutines.
    let looping = cs(&[&nums(&[-107]), &[10], &[11]]);
    let chain: Vec<Vec<u8>> = (0..30).map(|i| if i < 29 { cs(&[&nums(&[-107 + i + 1]), &[29], &[11]]) } else { cs(&[&[11]]) }).collect();
    let font = cff_font(
        &["loop", "missing", "chain"],
        &[cs(&[&[14]]), cs(&[&nums(&[-107]), &[10], &[14]]), cs(&[&nums(&[500]), &[10], &[14]]), cs(&[&nums(&[-107]), &[29], &[14]])],
        &chain,
        &[looping],
    );
    let cff = load_cff(font).unwrap();
    assert_eq!(cff_bounds(&cff, 1).err(), Some("subroutines nested too deep"));
    assert!(cff_bounds(&cff, 2).is_err());
    assert_eq!(cff_bounds(&cff, 3).err(), Some("subroutines nested too deep"));
    // The stack: 200 numbers.
    let many: Vec<i32> = vec![1; 200];
    let font = cff_font(&["stack"], &[cs(&[&[14]]), cs(&[&nums(&many), &[5], &[14]])], &[], &[]);
    assert_eq!(cff_bounds(&load_cff(font).unwrap(), 1).err(), Some("the charstring stack overflows"));
    // A charstring of a million operators (rlineto with arguments) ends by the tick limit, not by running a million segments.
    let mut long = nums(&[0, 0]);
    long.push(21);
    for _ in 0..400_000 {
        long.extend(nums(&[1, 1]));
        long.push(5);
    }
    long.push(14);
    let font = cff_font(&["long"], &[cs(&[&[14]]), long], &[], &[]);
    let r = cff_bounds(&load_cff(font).unwrap(), 1);
    assert!(r.is_err(), "{r:?}");
    // 100,000 segments are over the limit of one outline.
    let mut segs = nums(&[0, 0]);
    segs.push(21);
    for _ in 0..30_000 {
        segs.extend(nums(&[1, 1, 1, 1]));
        segs.push(5);
    }
    segs.push(14);
    let font = cff_font(&["segs"], &[cs(&[&[14]]), segs], &[], &[]);
    assert_eq!(cff_bounds(&load_cff(font).unwrap(), 1).err(), Some("the glyph outline has too many segments"));
    // A hintmask that runs past the end; a number cut short.
    let font = cff_font(&["cut"], &[cs(&[&[14]]), cs(&[&nums(&[1, 2]), &[1], &[19]]), cs(&[&[28, 1]])], &[], &[]);
    let cff = load_cff(font).unwrap();
    assert!(cff_bounds(&cff, 1).is_err());
    assert!(cff_bounds(&cff, 2).is_err());
    // Structure: an INDEX whose last offset is far away; a charset that claims more than the data has.
    let mut bad = cff_font(&["one"], &[cs(&[&[14]]), square(100)], &[], &[]);
    let at = bad.len() - 60;
    bad[at..at + 4].copy_from_slice(&be32(0xFFFF_FF00));
    let _ = load_cff(bad.clone());
    for len in (0..bad.len()).step_by(3) {
        if let Ok(f) = load_cff(bad[..len].to_vec()) {
            let _ = cff_bounds(&f, 1);
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn hostile_type1_eexec_subr_loops_and_sizes() {
    let started = Instant::now();
    // The encrypted part is noise: an error, not a hang.
    let mut noise = b"%!PS-AdobeFont-1.0\n/FontMatrix [0.001 0 0 0.001 0 0] def\ncurrentfile eexec\n".to_vec();
    let mut x = 1u32;
    for _ in 0..50_000 {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        noise.push((x >> 24) as u8 | 0x80);
    }
    assert!(load_t1(&noise).is_err());
    assert!(load_t1(b"%!PS-AdobeFont-1.0\n").is_err());
    assert!(load_t1(b"").is_err());
    assert!(load_t1(&[0x80, 1, 255, 255, 255, 127]).is_err());
    // A subroutine that calls itself, a missing subroutine, a charstring that cuts off in a number.
    let looping = cs(&[&t1_num(0), &[10], &[11]]);
    let font = type1(
        &[(".notdef", cs(&[&[14]])), ("A", cs(&[&t1_num(0), &[10], &[14]])), ("B", cs(&[&t1_num(9), &[10], &[14]])), ("C", cs(&[&[255, 1]]))],
        &[looping],
        &[(65, "A")],
    );
    let t1 = load_t1(&font).unwrap();
    assert_eq!(t1_bounds(&t1, t1.gid_of_name("A").unwrap()).err(), Some("subroutines nested too deep"));
    assert!(t1_bounds(&t1, t1.gid_of_name("B").unwrap()).is_err());
    assert!(t1_bounds(&t1, t1.gid_of_name("C").unwrap()).is_err());
    // A seac whose accent is the glyph itself ends after one level.
    let font = type1(&[(".notdef", cs(&[&[14]])), ("A", cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 0, 0, 65, 65]), &[12, 6]]))], &[], &[(65, "A")]);
    let t1 = load_t1(&font).unwrap();
    assert_eq!(t1_bounds(&t1, t1.gid_of_name("A").unwrap()).err(), Some("seac inside seac"));
    // A million operators.
    let mut long = cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 0]), &[21]]);
    for _ in 0..200_000 {
        long.extend(t1_nums(&[1, 1]));
        long.push(5);
    }
    let font = type1(&[(".notdef", cs(&[&[14]])), ("A", long)], &[], &[(65, "A")]);
    let t1 = load_t1(&font).unwrap();
    assert!(t1_bounds(&t1, t1.gid_of_name("A").unwrap()).is_err());
    // A claimed CharString longer than the data; a Subr number of two billion; a stack of hundreds.
    let mut font = type1(&[(".notdef", cs(&[&[14]])), ("A", t1_square(100))], &[cs(&[&[11]])], &[(65, "A")]);
    let at = font.windows(5).position(|w| w == b"eexec").unwrap() + 6;
    // Corrupting the private part makes it garbage after decryption: still an error or nothing.
    for i in (at..font.len()).step_by(11) {
        font[i] ^= 0x55;
    }
    if let Ok(t) = load_t1(&font) {
        for g in 0..3 {
            let _ = t1_bounds(&t, g);
        }
    }
    let many = t1_nums(&vec![1; 300]);
    let font = type1(&[(".notdef", cs(&[&[14]])), ("A", cs(&[&t1_nums(&[0, 500]), &[13], &many, &[14]]))], &[], &[(65, "A")]);
    let t1 = load_t1(&font).unwrap();
    assert_eq!(t1_bounds(&t1, t1.gid_of_name("A").unwrap()).err(), Some("the charstring stack overflows"));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

// --- pages -------------------------------------------------------------------------------------------------

/// A page with one embedded font (object 5) and the content given.
fn text_page(font_dict: &str, descriptor_extra: &str, program: &[u8], program_key: &str, content: &str) -> super::Document {
    let font = format!("<< /Type /Font {font_dict} /FontDescriptor 6 0 R >>");
    let descriptor = format!("<< /Type /FontDescriptor /FontName /Test {descriptor_extra} /{program_key} 7 0 R >>");
    page_doc("", "<< /Font << /F1 5 0 R >> >>", content.as_bytes(), &[(5, &font), (6, &descriptor)], &[(7, "", program)])
}

const SIMPLE_TT: &str = "/Subtype /TrueType /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding /WinAnsiEncoding";

#[test]
fn an_embedded_truetype_font_is_drawn_in_every_text_rendering_mode() {
    let page = |content: &str| {
        let doc = text_page(SIMPLE_TT, "/Flags 32", &test_truetype(), "FontFile2", content);
        let (b, w) = draw(&doc);
        assert!(w.is_empty(), "{w:?}");
        b.unwrap()
    };
    // The glyph A is a square of 800 units: at 100 points from the origin it covers x 0 to 80 and y 20 to 100 on the picture.
    let fill = page("BT /F1 100 Tf 0 0 Td 1 0 0 rg (A) Tj ET");
    assert_eq!(fill.boxed_characters, 0);
    assert_eq!(pixel(&fill, 40, 60), [255, 0, 0]);
    assert_eq!(pixel(&fill, 90, 60), WHITE);
    assert_eq!(pixel(&fill, 40, 10), WHITE);
    // Mode 3: nothing. Mode 1: the outline only. Mode 2: both.
    assert_eq!(pixel(&page("BT /F1 100 Tf 3 Tr 0 0 Td (A) Tj ET"), 40, 60), WHITE);
    let stroke = page("BT /F1 100 Tf 1 Tr 4 w 0 0 1 RG 0 0 Td (A) Tj ET");
    assert_eq!(pixel(&stroke, 40, 60), WHITE);
    assert_eq!(pixel(&stroke, 80, 60), [0, 0, 255]);
    let both = page("BT /F1 100 Tf 2 Tr 4 w 0 0 1 RG 1 0 0 rg 0 0 Td (A) Tj ET");
    assert_eq!(pixel(&both, 40, 60), [255, 0, 0]);
    assert_eq!(pixel(&both, 80, 60), [0, 0, 255]);
    // Mode 7 clips and draws nothing; the fill that follows shows only inside the glyph. Mode 4 fills and clips.
    let clip = page("BT /F1 100 Tf 7 Tr 0 0 Td (A) Tj ET 0 0 1 rg 0 0 100 100 re f");
    assert_eq!(pixel(&clip, 40, 60), [0, 0, 255]);
    assert_eq!(pixel(&clip, 90, 60), WHITE);
    assert_eq!(pixel(&clip, 40, 10), WHITE);
    let fill_clip = page("BT /F1 100 Tf 4 Tr 1 0 0 rg 0 0 Td (A) Tj ET 0 0 1 rg 0 0 100 100 re f");
    assert_eq!(pixel(&fill_clip, 40, 60), [0, 0, 255]);
    assert_eq!(pixel(&fill_clip, 90, 60), WHITE);
    // Two glyphs clip together; B is the bar (x 100 to 300, y 0 to 900 units) at 80 units to the right of A's advance.
    let two = page("BT /F1 50 Tf 7 Tr 0 0 Td (AB) Tj ET 0 1 0 rg 0 0 100 100 re f");
    assert_eq!(pixel(&two, 20, 80), [0, 255, 0]);
    assert_eq!(pixel(&two, 60, 80), [0, 255, 0]);
    assert_eq!(pixel(&two, 45, 80), WHITE);
    // Horizontal scaling, rise and word spacing move the glyph; a rotated text matrix turns it.
    let scaled = page("BT /F1 50 Tf 50 Tz 10 Ts 0 0 Td (A) Tj ET");
    assert_eq!(pixel(&scaled, 10, 52), [0, 0, 0]);
    assert_eq!(pixel(&scaled, 30, 52), WHITE);
    let turned = page("BT /F1 50 Tf 0 1 -1 0 60 0 Tm (A) Tj ET");
    // Turned by 90 degrees counter-clockwise about (60, 0): the square covers x 20 to 60, y 0 to 40 points.
    assert_eq!(pixel(&turned, 40, 80), [0, 0, 0]);
    assert_eq!(pixel(&turned, 40, 40), WHITE);
}

#[test]
fn glyph_choice_follows_the_truetype_rules() {
    // A symbolic font reads the (3,0) cmap at 0xF000 and up: code 65 is glyph 2 here, the tall bar.
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800), tt_rect(100, 0, 300, 900)], cmap: cmap_format4(3, 0, &[(0xF041, 2)]), long_loca: false }.build();
    let doc = text_page("/Subtype /TrueType /BaseFont /Test /FirstChar 65 /LastChar 65 /Widths [1000]", "/Flags 4", &font, "FontFile2", "BT /F1 100 Tf 0 0 Td (A) Tj ET");
    let b = draw(&doc).0.unwrap();
    assert_eq!(pixel(&b, 20, 50), [0, 0, 0]);
    assert_eq!(pixel(&b, 60, 50), WHITE);
    // A non-symbolic font with Differences goes by the glyph name through Unicode: /B is U+0042, which the cmap maps to glyph 2.
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800), tt_rect(100, 0, 300, 900)], cmap: cmap_format4(3, 1, &[(65, 1), (66, 2)]), long_loca: false }.build();
    let dict = "/Subtype /TrueType /BaseFont /Test /FirstChar 65 /LastChar 65 /Widths [1000] /Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [65 /B] >>";
    let doc = text_page(dict, "/Flags 32", &font, "FontFile2", "BT /F1 100 Tf 0 0 Td (A) Tj ET");
    let b = draw(&doc).0.unwrap();
    assert_eq!(pixel(&b, 20, 50), [0, 0, 0]);
    assert_eq!(pixel(&b, 60, 50), WHITE);
}

#[test]
fn embedded_type1_and_cff_fonts_use_their_glyph_names_and_encodings() {
    let t1 = type1(&[(".notdef", cs(&[&[14]])), ("A", t1_square(800)), ("bar", cs(&[&t1_nums(&[100, 500]), &[13], &t1_nums(&[0, 0]), &[21], &t1_nums(&[200]), &[6], &t1_nums(&[900]), &[7], &t1_nums(&[-200]), &[6], &[9], &[14]]))], &[], &[(65, "A")]);
    // The built-in encoding gives A the square.
    let doc = text_page("/Subtype /Type1 /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000]", "/Flags 4", &t1, "FontFile", "BT /F1 100 Tf 0 0 Td (A) Tj ET");
    let (b, w) = draw(&doc);
    let b = b.unwrap();
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 40, 60), [0, 0, 0]);
    assert_eq!(pixel(&b, 90, 60), WHITE);
    // /Differences renames code 65 to the bar.
    let doc = text_page(
        "/Subtype /Type1 /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding << /Type /Encoding /Differences [65 /bar] >>",
        "/Flags 4",
        &t1,
        "FontFile",
        "BT /F1 100 Tf 0 0 Td (A) Tj ET",
    );
    let b = draw(&doc).0.unwrap();
    assert_eq!(pixel(&b, 20, 50), [0, 0, 0]);
    assert_eq!(pixel(&b, 60, 50), WHITE);
    // The same through a CFF font (FontFile3): glyph names "bar" and the standard A (SID 34).
    let a_sid = crate::text::fontprog::standard_encoding_sid(65).unwrap();
    let bar = cs(&[&nums(&[100, 0]), &[21], &nums(&[200]), &[6], &nums(&[900]), &[7], &nums(&[-200]), &[6], &[14]]);
    let mut cff = cff_font(&["x", "bar"], &[cs(&[&[14]]), square(800), bar], &[], &[]);
    let at = cff.windows(5).position(|w| w == [0, 0x01, 0x87, 0x01, 0x88]).expect("charset");
    cff[at + 1..at + 3].copy_from_slice(&be16(a_sid));
    let doc = text_page("/Subtype /Type1 /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding /WinAnsiEncoding", "/Flags 32", &cff, "FontFile3", "BT /F1 100 Tf 0 0 Td (A) Tj ET");
    let (b, w) = draw(&doc);
    let b = b.unwrap();
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 40, 60), [0, 0, 0]);
}

#[test]
fn composite_fonts_pick_glyphs_by_cid_and_write_vertically() {
    // CIDFontType2, Identity-H, the glyph of a CID is the glyph with that number (or the CIDToGIDMap's).
    let descendant = "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /DW 1000 %MAP% >>";
    let page = |wmode: &str, map: &str, content: &str| {
        let font = format!("<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /{wmode} /DescendantFonts [8 0 R] >>");
        let desc = descendant.replace("%MAP%", map);
        let descriptor = "<< /Type /FontDescriptor /FontName /Test /Flags 4 /FontFile2 7 0 R >>";
        page_doc("", "<< /Font << /F1 5 0 R >> >>", content.as_bytes(), &[(5, &font), (6, descriptor), (8, &desc)], &[(7, "", &test_truetype())])
    };
    let b = draw(&page("Identity-H", "/CIDToGIDMap /Identity", "BT /F1 100 Tf 0 0 Td <0002> Tj ET")).0.unwrap();
    assert_eq!(pixel(&b, 20, 50), [0, 0, 0]);
    assert_eq!(pixel(&b, 60, 50), WHITE);
    // A map stream sends CID 5 to glyph 1 (the square).
    let doc = {
        let font = "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H /DescendantFonts [8 0 R] >>";
        let desc = descendant.replace("%MAP%", "/CIDToGIDMap 9 0 R");
        let descriptor = "<< /Type /FontDescriptor /FontName /Test /Flags 4 /FontFile2 7 0 R >>";
        let mut map = vec![0u8; 12];
        map[10..12].copy_from_slice(&be16(1));
        page_doc("", "<< /Font << /F1 5 0 R >> >>", b"BT /F1 100 Tf 0 0 Td <0005> Tj ET", &[(5, font), (6, descriptor), (8, &desc)], &[(7, "", &test_truetype()), (9, "", &map)])
    };
    let b = draw(&doc).0.unwrap();
    assert_eq!(pixel(&b, 40, 60), [0, 0, 0]);
    assert_eq!(pixel(&b, 90, 60), WHITE);
    // Vertical writing: the glyph origin is the pen less the position vector (w0 / 2, 880 units): at 50 points and pen
    // (50, 100) that is (25, 56), and the square (800 units, 40 points) spans x 25 to 65 and y 56 to 96 (picture rows 4 to 44).
    let b = draw(&page("Identity-V", "/CIDToGIDMap /Identity", "BT /F1 50 Tf 50 100 Td <0001> Tj ET")).0.unwrap();
    assert_eq!(pixel(&b, 45, 24), [0, 0, 0]);
    assert_eq!(pixel(&b, 5, 24), WHITE);
    assert_eq!(pixel(&b, 45, 60), WHITE);
}

#[test]
fn a_font_program_that_cannot_be_read_is_a_warning_and_the_text_still_has_a_font() {
    for (key, program) in [("FontFile2", b"this is not a font program at all".to_vec()), ("FontFile3", vec![1, 0, 4, 4, 0, 0]), ("FontFile", b"%!PS-AdobeFont-1.0\nnothing".to_vec())] {
        let doc = text_page(SIMPLE_TT, "/Flags 32", &program, key, "BT /F1 40 Tf 10 10 Td (AB) Tj ET");
        let (b, w) = draw(&doc);
        let b = b.expect("the page is drawn");
        assert!(w.iter().any(|m| m.contains("font program was not read")), "{key}: {w:?}");
        // A system font stands in (or, where there is none, boxes do): in either case something is on the page.
        assert!(b.boxed_characters > 0 || b.rgba.chunks_exact(4).any(|p| p[0] < 128), "{key}");
    }
    // One broken glyph: the other glyphs of the font are drawn and the page carries on with a warning.
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_composite(&[(1, 0, 0)]), tt_rect(100, 0, 300, 900)], cmap: cmap_format4(3, 1, &[(65, 1), (66, 2)]), long_loca: false }.build();
    let doc = text_page(SIMPLE_TT, "/Flags 32", &font, "FontFile2", "BT /F1 50 Tf 0 0 Td (AB) Tj ET");
    let (b, w) = draw(&doc);
    let b = b.unwrap();
    assert!(w.iter().any(|m| m.contains("a glyph could not be read")), "{w:?}");
    assert_eq!(pixel(&b, 60, 80), [0, 0, 0]);
}

#[test]
fn glyph_and_program_caches_stay_within_their_budgets() {
    // Thousands of different sizes of two glyphs: the bitmap cache is bounded.
    let mut content = String::from("BT\n");
    for i in 0..6000 {
        let size = 8.0 + f64::from(i) * 0.013;
        content.push_str(&format!("/F1 {size:.3} Tf 1 0 0 1 {} {} Tm (AB) Tj\n", 5 + i % 60, 5 + i / 60 % 90));
    }
    content.push_str("ET");
    let doc = text_page(SIMPLE_TT, "/Flags 32", &test_truetype(), "FontFile2", &content);
    let mut r = super::Renderer::new(&doc);
    let pages = doc.pages().unwrap();
    r.render_page(&pages[0], 300.0).unwrap();
    let (bitmaps, glyphs, programs) = r.cache_bytes_for_test();
    assert!(bitmaps <= 9 << 20, "bitmaps {bitmaps}");
    assert!(glyphs <= 17 << 20, "glyph outlines {glyphs}");
    assert!(programs <= 65 << 20, "programs {programs}");
    assert!(programs > 0 && glyphs > 0);
}

#[test]
fn a_page_that_shows_millions_of_glyphs_is_stopped_by_the_operator_budget() {
    let doc = text_page(SIMPLE_TT, "/Flags 32", &test_truetype(), "FontFile2", &format!("BT /F1 1 Tf {} ET", "(AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA) Tj
".repeat(30_000)));
    let started = Instant::now();
    assert!(matches!(draw(&doc).0, Err(crate::Error::Limit(_))));
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn gbk_codes_and_standard_encoding_names() {
    assert_eq!(super::fonts::gbk_to_unicode(0xCBCE), 0x5B8B);
    assert_eq!(super::fonts::gbk_to_unicode(0xCCE5), 0x4F53);
    assert_eq!(super::fonts::gbk_to_unicode(0x8100), 0);
    assert!(super::sysfont::is_gbk_font_name(b"\xCB\xCE\xCC\xE5"));
    assert!(!super::sysfont::is_gbk_font_name(b"SimSun"));
}

#[test]
fn glyphs_with_huge_outlines_are_charged_to_the_page_and_odd_render_modes_are_ignored() {
    // A glyph of 30,000 points shown 3,000 times is 90 million segments: the page stops drawing glyphs and says so.
    let points: Vec<(i16, i16, bool)> = (0..30_000).map(|i| ((i % 1000) as i16, (i / 30) as i16, true)).collect();
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_glyph(&[&points])], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: true }.build();
    let doc = text_page(SIMPLE_TT, "/Flags 32", &font, "FontFile2", &format!("BT /F1 1 Tf 1 0 0 1 10 10 Tm {} ET", "(A) Tj\n".repeat(3000)));
    let started = Instant::now();
    let (b, w) = draw(&doc);
    b.unwrap();
    assert!(w.iter().any(|m| m.contains("more outline segments")), "{w:?}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    // Render mode 9 is no mode: the glyph is filled as before.
    let doc = text_page(SIMPLE_TT, "/Flags 32", &test_truetype(), "FontFile2", "BT /F1 100 Tf 9 Tr 0 0 Td (A) Tj ET");
    assert_eq!(pixel(&draw(&doc).0.unwrap(), 40, 60), [0, 0, 0]);
}

// --- after the review of 3b: work and memory a hostile font may cost ---------------------------------------------

/// A page with a composite font (Identity-H, every glyph `dw` wide) whose descendant has the program given.
fn cid_page(cid_subtype: &str, program_key: &str, program: &[u8], dw: u32, content: &str) -> super::Document {
    let cid = format!("<< /Type /Font /Subtype /{cid_subtype} /BaseFont /X /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 8 0 R /DW {dw} /CIDToGIDMap /Identity >>");
    let descriptor = format!("<< /Type /FontDescriptor /FontName /X /Flags 4 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 900 /Descent 0 /CapHeight 900 /StemV 80 /{program_key} 7 0 R >>");
    let root = "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [6 0 R] >>";
    page_doc("", "<< /Font << /F1 5 0 R >> >>", content.as_bytes(), &[(5, root), (6, &cid), (8, &descriptor)], &[(7, "", program)])
}

/// A string of two byte codes.
fn hex_codes(codes: std::ops::RangeInclusive<u32>) -> String {
    let mut s = String::from("<");
    for c in codes {
        s.push_str(&format!("{c:04X}"));
    }
    s.push('>');
    s
}

#[test]
fn a_big_type1_subroutine_called_again_and_again_is_charged_not_decrypted_each_time() {
    let started = Instant::now();
    // A subroutine of 64 KB that returns at once, called 33,000 times by one glyph (3.4 seconds before).
    let mut big = vec![11u8];
    big.extend(vec![0u8; 65_536]);
    let mut many = cs(&[&t1_nums(&[0, 500]), &[13]]);
    for _ in 0..33_000 {
        many.extend(t1_num(0));
        many.push(10);
    }
    many.push(14);
    let once = cs(&[&t1_nums(&[0, 500]), &[13], &t1_nums(&[0, 0]), &[21], &t1_num(0), &[10], &t1_num(100), &[6], &t1_num(100), &[7], &[9], &[14]]);
    let font = type1(&[(".notdef", cs(&[&[14]])), ("A", many), ("B", once)], &[big], &[(65, "A"), (66, "B")]);
    let t1 = load_t1(&font).unwrap();
    let mut b = t1.builder();
    assert_eq!(t1.outline(t1.gid_of_name("A").unwrap(), &mut b).err(), Some("the charstring runs too long"));
    assert!(b.work() <= 400_000, "work {}", b.work());
    // One call is within the limit and still draws.
    let bounds = t1_bounds(&t1, t1.gid_of_name("B").unwrap()).unwrap().unwrap();
    assert_bounds(bounds, 0.0, 0.0, 0.1, 0.1);
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

#[test]
fn glyphs_that_each_run_to_the_tick_limit_are_cut_off_by_the_pages_outline_budget() {
    // Subroutine k calls subroutine k-1 four times: a glyph that calls the top one makes 4^8 calls and fails at the limit
    // of 200,000 ticks. 3,000 such glyphs took 4.6 seconds; the page may spend 20 million.
    let mut gsubrs = vec![vec![11u8]];
    for k in 1..=8 {
        let mut s = Vec::new();
        for _ in 0..4 {
            s.extend(cff_num(k - 1 - 107));
            s.push(29);
        }
        s.push(11);
        gsubrs.push(s);
    }
    let glyph = cs(&[&cff_num(8 - 107), &[29], &[14]]);
    let n = 3000usize;
    let names: Vec<String> = (0..n).map(|i| format!("g{i}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let font = cff_font(&name_refs, &vec![glyph; n + 1], &gsubrs, &[]);
    let one = load_cff(font.clone()).unwrap();
    assert_eq!(cff_bounds(&one, 1).err(), Some("the charstring runs too long"));
    let doc = cid_page("CIDFontType0", "FontFile3", &font, 1, &format!("BT /F1 12 Tf 0 50 Td {} Tj ET", hex_codes(1..=n as u32)));
    let started = Instant::now();
    let (b, w) = draw(&doc);
    b.unwrap();
    assert!(w.iter().any(|m| m.contains("took more work than a page may spend")), "{w:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn glyph_bitmaps_are_held_to_their_budget_as_they_are_made_and_off_page_glyphs_make_none() {
    // 20,000 different glyphs of 72 by 72 pixels in one string: 100 MB if all were kept. (A glyph is a composite of one.)
    let n = 20_000usize;
    let mut glyphs = vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800)];
    glyphs.extend((2..n).map(|_| tt_composite(&[(1, 0, 0)])));
    let font = Tt { glyphs, cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: true }.build();
    let codes = hex_codes(1..=(n - 1) as u32);
    let render = |content: String| {
        let doc = cid_page("CIDFontType2", "FontFile2", &font, 0, &content);
        let mut r = super::Renderer::new(&doc);
        r.render_page(&doc.pages().unwrap()[0], 72.0).unwrap();
        r.bitmap_peak_for_test()
    };
    let started = Instant::now();
    let peak = render(format!("BT /F1 90 Tf 0 10 Td {codes} Tj ET"));
    assert!(peak > 1 << 20 && peak <= 9 << 20, "peak {peak}");
    // The same string far to the left of the page: nothing is rasterized.
    assert_eq!(render(format!("BT /F1 90 Tf 1 0 0 1 -5000 10 Tm {codes} Tj ET")), 0);
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

#[test]
fn truetype_contour_ends_are_counted_against_the_point_limit() {
    // Glyph 1 has 32,767 contours of one point; glyph 2 includes it 256 times (71 MB of contour ends before).
    let mut many = Vec::new();
    many.extend(32767i16.to_be_bytes());
    many.extend([0u8; 8]);
    for _ in 0..32767 {
        many.extend(be16(0));
    }
    many.extend(be16(0));
    many.push(1);
    many.extend(5i16.to_be_bytes());
    many.extend(5i16.to_be_bytes());
    let data = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), many, tt_composite(&vec![(1, 0, 0); 256])], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: true }.build();
    let tt = load_tt(data).unwrap();
    let started = Instant::now();
    assert!(tt_bounds(&tt, 1).is_ok());
    assert_eq!(tt_bounds(&tt, 2).err(), Some("a glyph has too many points"));
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

#[test]
fn the_clip_of_a_text_object_has_a_cap_on_its_outline_segments() {
    // A glyph of 39,990 off-curve points shown 200 times in mode 7: the clip would be 8 million segments.
    let n = 39_990usize;
    let points: Vec<(i16, i16, bool)> = (0..n)
        .map(|i| {
            let a = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            ((500.0 + 400.0 * a.cos()) as i16, (500.0 + 400.0 * a.sin()) as i16, false)
        })
        .collect();
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_glyph(&[&points])], cmap: cmap_format4(3, 1, &[(65, 1)]), long_loca: true }.build();
    let content = format!("BT /F1 40 Tf 7 Tr 0 50 Td <{}> Tj ET 1 0 0 rg 0 0 100 100 re f", "0001".repeat(200));
    let doc = cid_page("CIDFontType2", "FontFile2", &font, 1, &content);
    let started = Instant::now();
    let (b, w) = draw(&doc);
    b.unwrap();
    assert!(w.iter().any(|m| m.contains("do not add to the clip")), "{w:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn direct_font_dictionaries_and_shared_font_files_are_read_once() {
    let font = test_truetype();
    let dict = "<< /Type /Font /Subtype /TrueType /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>";
    let resources = format!("<< /Font << /F1 {dict} /F2 {dict} /F3 5 0 R >> >>");
    let mut content = String::from("BT ");
    for _ in 0..500 {
        content.push_str("/F1 12 Tf (A) Tj /F2 12 Tf (B) Tj /F3 12 Tf (A) Tj ");
    }
    content.push_str("ET");
    let descriptor = "<< /Type /FontDescriptor /FontName /Test /Flags 32 /FontFile2 7 0 R >>";
    let doc = page_doc("", &resources, content.as_bytes(), &[(5, dict), (6, descriptor)], &[(7, "", &font)]);
    let mut r = super::Renderer::new(&doc);
    r.render_page(&doc.pages().unwrap()[0], 72.0).unwrap();
    // Three readers (one per font dictionary, the direct ones kept for the next Tf), one parse of the shared FontFile2.
    assert_eq!(r.font_entries_for_test(), 3);
    assert_eq!(r.cache_bytes_for_test().2, font.len());
}

#[test]
fn big_fonts_used_in_turn_stop_being_read_again_when_the_page_has_read_enough() {
    // Three programs of 20 MB (a few KB of font, the rest zeros, compressed): past 32 MB held, each new one makes the others
    // give their memory back, so strings in turn read them again and again. The page may read 128 MB.
    let mut dicts = Vec::new();
    for i in 0..3u32 {
        dicts.push((10 + i, format!("<< /Type /Font /Subtype /TrueType /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding /WinAnsiEncoding /FontDescriptor {} 0 R >>", 20 + i)));
        dicts.push((20 + i, format!("<< /Type /FontDescriptor /FontName /Test /Flags 32 /FontFile2 {} 0 R >>", 30 + i)));
    }
    let mut program = test_truetype();
    program.extend(vec![0u8; 20 << 20]);
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&program, 6);
    drop(program);
    let mut content = String::from("BT ");
    for _ in 0..10 {
        content.push_str("/F0 12 Tf (A) Tj /F1 12 Tf (A) Tj /F2 12 Tf (A) Tj ");
    }
    content.push_str("ET");
    let objs: Vec<(u32, &str)> = dicts.iter().map(|(n, d)| (*n, d.as_str())).collect();
    let streams: Vec<(u32, &str, &[u8])> = (0..3).map(|i| (30 + i, "/Filter /FlateDecode", packed.as_slice())).collect();
    let doc = page_doc("", "<< /Font << /F0 10 0 R /F1 11 0 R /F2 12 0 R >> >>", content.as_bytes(), &objs, &streams);
    let started = Instant::now();
    let (b, w) = draw(&doc);
    b.unwrap();
    assert!(w.iter().any(|m| m.contains("has read more font program data than a page may")), "{w:?}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

#[test]
fn truetype_glyphs_are_chosen_through_the_encoding_as_9_6_6_4_says() {
    // Code 0x80 is the Euro in WinAnsi: through the encoding it is the square (cmap U+20AC); straight from the cmap it is the bar.
    let font = Tt { glyphs: vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800), tt_rect(100, 0, 300, 900)], cmap: cmap_format4(3, 1, &[(0x80, 2), (0x20AC, 1)]), long_loca: false }.build();
    let square = |encoding: &str, flags: &str| {
        let dict = format!("/Subtype /TrueType /BaseFont /Test /FirstChar 128 /LastChar 128 /Widths [1000] {encoding}");
        let doc = text_page(&dict, flags, &font, "FontFile2", "BT /F1 100 Tf 0 0 Td (\\200) Tj ET");
        pixel(&draw(&doc).0.unwrap(), 60, 50) == [0, 0, 0]
    };
    // A named WinAnsiEncoding without /Differences: through the encoding, whatever the flags say.
    assert!(square("/Encoding /WinAnsiEncoding", "/Flags 4"));
    assert!(square("/Encoding /WinAnsiEncoding", "/Flags 32"));
    // The Nonsymbolic flag: through the encoding even with no /Encoding, and with /Differences alone.
    assert!(square("", "/Flags 32"));
    assert!(square("/Encoding << /Type /Encoding /Differences [65 /B] >>", "/Flags 32"));
    // Symbolic (or no flags): the cmap as it is.
    assert!(!square("", "/Flags 4"));
    assert!(!square("/Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [65 /B] >>", "/Flags 4"));
    assert!(!square("/Encoding << /Type /Encoding /Differences [65 /B] >>", "/Flags 4"));
}

#[test]
fn gbk_pairs_apply_only_to_fonts_with_no_program_at_all() {
    let dict = "/Subtype /TrueType /BaseFont /#CB#CE#CC#E5 /FirstChar 65 /LastChar 65 /Widths [500]";
    let gbk_pairs = |doc: &super::Document| {
        let Object::Dict(d) = doc.get(ObjRef::new(5, 0)).unwrap() else { panic!("no font dictionary") };
        let font = Font::load(doc, &d, &mut HashMap::new(), &mut Warnings::default());
        GlyphSource::new(doc, &d, &font).gbk_pairs
    };
    let font = format!("<< /Type /Font {dict} /FontDescriptor 6 0 R >>");
    let bare = page_doc("", "<< /Font << /F1 5 0 R >> >>", b"", &[(5, &font), (6, "<< /Type /FontDescriptor /FontName /Test /Flags 32 >>")], &[]);
    assert!(gbk_pairs(&bare));
    for key in ["FontFile", "FontFile2", "FontFile3"] {
        assert!(!gbk_pairs(&text_page(dict, "/Flags 32", b"x", key, "")), "{key}");
    }
}

#[test]
fn cff_fdselect_format_3_is_read_in_one_pass_and_bad_ranges_are_ignored() {
    use super::cff::read_fd_select;
    let started = Instant::now();
    // 65,535 ranges that alternate between "all glyphs" and nothing: the first one wins, the rest are not walked.
    let n = 65_535usize;
    let mut d = vec![3u8];
    d.extend(be16(65_535));
    for i in 0..65_535usize {
        d.extend(be16(if i % 2 == 0 { 0 } else { 65_535 }));
        d.push((i % 250 + 1) as u8);
    }
    d.extend(be16(65_535));
    let out = read_fd_select(&d, 0, n).unwrap();
    assert!(out.iter().all(|&v| v == 1));
    // A range that starts before the end of the one before, and one that ends before it starts, change nothing.
    let mut d = vec![3u8];
    d.extend(be16(3));
    for (first, fd) in [(0u16, 1u8), (5, 2), (3, 3)] {
        d.extend(be16(first));
        d.push(fd);
    }
    d.extend(be16(8));
    assert_eq!(read_fd_select(&d, 0, 8).unwrap(), vec![1, 1, 1, 1, 1, 0, 0, 0]);
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

// --- tricky fonts: glyphs made by their instructions (3b2) ----------------------------------------------------

/// NPUSHW: signed 16-bit values.
fn ins_words(v: &[i32]) -> Vec<u8> {
    let mut out = vec![0x41, v.len() as u8];
    for x in v {
        out.extend((*x as i16).to_be_bytes());
    }
    out
}

fn ins_cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

/// A composite glyph whose last component carries instructions.
fn tt_composite_ins(parts: &[(u16, i16, i16)], ins: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((-1i16).to_be_bytes());
    out.extend([0u8; 8]);
    for (i, (gid, dx, dy)) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        out.extend(be16(0x0003 | if last { 0x100 } else { 0x20 }));
        out.extend(be16(*gid));
        out.extend(dx.to_be_bytes());
        out.extend(dy.to_be_bytes());
    }
    out.extend(be16(ins.len() as u16));
    out.extend(ins);
    out
}

/// A name table with one record: platform 3, name id 1 (the family).
fn name_table(family: &str) -> Vec<u8> {
    let text: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut t = vec![0, 0, 0, 1, 0, 18];
    for v in [3u16, 1, 0x409, 1, text.len() as u16, 0] {
        t.extend(v.to_be_bytes());
    }
    t.extend(text);
    t
}

/// A TrueType font with 2048 units per em and the tables the instructions need.
fn tricky_font(glyphs: &[Vec<u8>], fpgm: &[u8], prep: &[u8], family: Option<&str>) -> Vec<u8> {
    let cvt: Vec<u8> = [100i16, 200, 300].iter().flat_map(|v| v.to_be_bytes()).collect();
    tricky_font_tables(glyphs, &cvt, fpgm, prep, family)
}

fn tricky_font_tables(glyphs: &[Vec<u8>], cvt: &[u8], fpgm: &[u8], prep: &[u8], family: Option<&str>) -> Vec<u8> {
    let mut glyf = Vec::new();
    let mut offsets = vec![0u32];
    for g in glyphs {
        glyf.extend(g);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
        offsets.push(glyf.len() as u32);
    }
    let loca: Vec<u8> = offsets.iter().flat_map(|o| be32(*o)).collect();
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&be16(2048));
    head[50..52].copy_from_slice(&be16(1));
    let mut maxp = vec![0u8; 32];
    maxp[..4].copy_from_slice(&be32(0x0001_0000));
    maxp[4..6].copy_from_slice(&be16(glyphs.len() as u16));
    for (at, v) in [(16usize, 4u16), (18, 16), (20, 8), (22, 0), (24, 64)] {
        maxp[at..at + 2].copy_from_slice(&be16(v));
    }
    let mut hhea = vec![0u8; 36];
    hhea[4..6].copy_from_slice(&be16(1800));
    hhea[6..8].copy_from_slice(&(-400i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&be16(1));
    let mut hmtx = Vec::new();
    hmtx.extend(be16(2048));
    hmtx.extend(be16(0));
    let cmap = cmap_format4(3, 1, &[(65, 1), (66, 2), (67, 3)]);
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> =
        vec![(b"cmap", cmap), (b"cvt ", cvt.to_vec()), (b"fpgm", fpgm.to_vec()), (b"glyf", glyf), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx), (b"loca", loca), (b"maxp", maxp), (b"prep", prep.to_vec())];
    if let Some(f) = family {
        tables.push((b"name", name_table(f)));
    }
    sfnt(&[0, 1, 0, 0], &tables)
}

const SQUARE_800: [(i16, i16, bool); 4] = [(0, 0, true), (0, 800, true), (800, 800, true), (800, 0, true)];

/// Glyph 1: a square of 800 units whose right side the instructions pull in to 100 units. Glyph 2: a tall bar, no instructions.
/// Glyph 3: a composite of glyph 1 moved 1000 units right.
fn narrowing_glyphs() -> Vec<Vec<u8>> {
    // SVTCA[x]; set the x of points 2 and 3 to 6400 (100 units in 26.6).
    let narrow = ins_cat(&[&[0x01], &ins_words(&[2, 6400]), &[0x48], &ins_words(&[3, 6400]), &[0x48]]);
    vec![tt_rect(0, 0, 0, 0), tt_glyph_ins(&[&SQUARE_800], &narrow), tt_rect(100, 0, 300, 900), tt_composite(&[(1, 1000, 0)])]
}

fn hinted_bounds(tt: &TtFont, gid: u32) -> Result<Option<Rect>, &'static str> {
    let mut b = Builder::new(tt.hinted_matrix());
    tt.outline_hinted(gid, &mut b)?;
    Ok(b.finish().map(|p| p.bounds()))
}

#[test]
fn tricky_fonts_are_known_by_their_name_table_and_their_pdf_name() {
    let tricky = load_tt(tricky_font(&narrowing_glyphs(), &[], &[], Some("DFKai-SB"))).unwrap();
    assert!(tricky.is_tricky());
    assert!(!load_tt(tricky_font(&narrowing_glyphs(), &[], &[], Some("Arial"))).unwrap().is_tricky());
    assert!(!load_tt(tricky_font(&narrowing_glyphs(), &[], &[], None)).unwrap().is_tricky());
    assert!(TtFont::is_tricky_name(b"ABCDEF+DFKaiShu-SB-Estd-BF"));
    assert!(TtFont::is_tricky_name(b"MingLiU"));
    assert!(!TtFont::is_tricky_name(b"Arial"));
    assert!(!TtFont::is_tricky_name(b"\xCB\xCE\xCC\xE5"));
}

#[test]
fn instructions_put_the_strokes_of_a_tricky_glyph_in_place() {
    let tt = load_tt(tricky_font(&narrowing_glyphs(), &[], &[], Some("DFKai-SB"))).unwrap();
    // Without instructions the square is 800 units wide (0.39 em); with them its right side is at 100 units (0.049 em).
    let plain = tt_bounds(&tt, 1).unwrap().unwrap();
    assert!((plain.right() - 800.0 / 2048.0).abs() < 1e-4);
    let hinted = hinted_bounds(&tt, 1).unwrap().unwrap();
    assert!((hinted.right() - 100.0 / 2048.0).abs() < 1e-3, "{hinted:?}");
    // (Rect::top is the smallest y, bottom the largest: the em has y up.)
    assert!((hinted.bottom() - 800.0 / 2048.0).abs() < 1e-3 && hinted.left().abs() < 1e-3 && hinted.top().abs() < 1e-3);
    // A glyph with no instructions comes out as it is.
    let bar = hinted_bounds(&tt, 2).unwrap().unwrap();
    assert!((bar.left() - 100.0 / 2048.0).abs() < 1e-3 && (bar.right() - 300.0 / 2048.0).abs() < 1e-3 && (bar.bottom() - 900.0 / 2048.0).abs() < 1e-3);
    // A composite: each component is hinted first and then put in place.
    let comp = hinted_bounds(&tt, 3).unwrap().unwrap();
    assert!((comp.left() - 1000.0 / 2048.0).abs() < 1e-3 && (comp.right() - 1100.0 / 2048.0).abs() < 1e-3, "{comp:?}");
}

#[test]
fn a_composite_glyphs_own_instructions_see_the_hinted_components() {
    // The composite (the tall bar moved 50 right) pulls its top two points (1 and 2) down to 100 units; the program sees
    // pixels (26.6), not font units, so the target is 6400.
    let program = ins_cat(&[&[0x00], &ins_words(&[1, 6400]), &[0x48], &ins_words(&[2, 6400]), &[0x48]]);
    let glyphs = vec![tt_rect(0, 0, 0, 0), tt_rect(0, 0, 0, 0), tt_rect(100, 0, 300, 900), tt_composite_ins(&[(2, 50, 0)], &program)];
    let tt = load_tt(tricky_font(&glyphs, &[], &[], Some("DFKai-SB"))).unwrap();
    let b = hinted_bounds(&tt, 3).unwrap().unwrap();
    assert!((b.bottom() - 100.0 / 2048.0).abs() < 1e-3 && b.top().abs() < 1e-3, "{b:?}");
    assert!((b.left() - 150.0 / 2048.0).abs() < 1e-3 && (b.right() - 350.0 / 2048.0).abs() < 1e-3, "{b:?}");
}

#[test]
fn font_programs_run_first_and_their_work_is_charged_once() {
    // fpgm defines function 0 (storage 0 += 1); prep calls it 3000 times; the glyph program puts the x of points 2 and 3 at storage 0.
    let body = ins_cat(&[&[0x40, 2, 0, 0], &[0x43, 0x40, 1, 1, 0x60, 0x42]]);
    let fpgm = ins_cat(&[&[0x40, 1, 0, 0x2C], &body, &[0x2D]]);
    let prep = ins_cat(&[&ins_words(&[3000, 0]), &[0x2A]]);
    let program = ins_cat(&[&[0x01, 0x40, 1, 2, 0x40, 1, 0, 0x43, 0x48], &[0x40, 1, 3, 0x40, 1, 0, 0x43, 0x48]]);
    let glyphs = vec![tt_rect(0, 0, 0, 0), tt_glyph_ins(&[&SQUARE_800], &program), tt_glyph_ins(&[&SQUARE_800], &program)];
    let tt = load_tt(tricky_font(&glyphs, &fpgm, &prep, Some("DFKai-SB"))).unwrap();
    let mut first = Builder::new(tt.hinted_matrix());
    tt.outline_hinted(1, &mut first).unwrap();
    let mut second = Builder::new(tt.hinted_matrix());
    tt.outline_hinted(2, &mut second).unwrap();
    // The control value program ran 3000 calls of a six-instruction function, and only for the first glyph.
    assert!(first.work() > 15_000, "{}", first.work());
    assert!(second.work() < 200, "{}", second.work());
    let right = first.finish().unwrap().bounds().right();
    assert!((right - 3000.0 / 64.0 / 2048.0).abs() < 1e-3, "{right}");
}

#[test]
fn hostile_instructions_fail_the_hinting_not_the_font() {
    // A glyph program that loops for ever; instructions that underflow the stack; one that overflows it.
    let endless = ins_cat(&[&ins_words(&[-4]), &[0x1C]]);
    let glyphs = vec![tt_rect(0, 0, 0, 0), tt_glyph_ins(&[&SQUARE_800], &endless), tt_glyph_ins(&[&SQUARE_800], &[0x60]), tt_glyph_ins(&[&SQUARE_800], &ins_cat(&[&[0x40, 1, 1], &[0x20], &ins_words(&[-5]), &[0x1C]]))];
    let tt = load_tt(tricky_font(&glyphs, &[], &[], Some("DFKai-SB"))).unwrap();
    for gid in 1..=3 {
        let mut b = Builder::new(tt.hinted_matrix());
        assert!(tt.outline_hinted(gid, &mut b).is_err(), "glyph {gid}");
        // The work the failed program did is still counted (the endless one used its whole allowance).
        if gid == 1 {
            assert!(b.work() >= 100_000, "{}", b.work());
        }
        // The plain outline of the glyph is still there.
        let plain = tt_bounds(&tt, gid).unwrap().unwrap();
        assert!((plain.right() - 800.0 / 2048.0).abs() < 1e-4);
    }
    // A control value program that never ends: the hinting of the font is off for good.
    let tt = load_tt(tricky_font(&[tt_rect(0, 0, 0, 0), tt_rect(0, 0, 800, 800)], &[], &endless, Some("DFKai-SB"))).unwrap();
    let mut b = Builder::new(tt.hinted_matrix());
    assert!(tt.outline_hinted(1, &mut b).is_err());
    assert!(b.work() >= 100_000, "{}", b.work());
    let mut b = Builder::new(tt.hinted_matrix());
    assert!(tt.outline_hinted(1, &mut b).is_err());
    assert!(b.work() < 100, "{}", b.work());
}

#[test]
fn random_damage_to_a_tricky_font_never_panics() {
    let mut state = 777u64;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let fpgm = ins_cat(&[&[0x40, 1, 0, 0x2C, 0x40, 2, 0, 0, 0x43, 0x40, 1, 1, 0x60, 0x42, 0x2D]]);
    let prep = ins_cat(&[&ins_words(&[20, 0]), &[0x2A]]);
    let good = tricky_font(&narrowing_glyphs(), &fpgm, &prep, Some("DFKai-SB"));
    let started = Instant::now();
    for round in 0..1500 {
        let mut d = good.clone();
        for _ in 0..1 + round % 5 {
            let at = next() % d.len();
            d[at] = next() as u8;
        }
        if round % 7 == 0 {
            d.truncate(next() % d.len());
        }
        if let Ok(tt) = load_tt(d) {
            for g in 0..5u32 {
                let mut b = Builder::new(tt.hinted_matrix());
                let _ = tt.outline_hinted(g, &mut b);
            }
        }
    }
    assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
}

const TRICKY_TT: &str = "/Subtype /TrueType /BaseFont /ABCDEF+DFKaiShu-SB /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding /WinAnsiEncoding";

#[test]
fn a_page_draws_a_tricky_font_with_its_instructions_and_no_other_font() {
    // The square A is pulled in to 100 units (4.9 pixels at 100 points) by its instructions.
    let font = tricky_font(&narrowing_glyphs(), &[], &[], None);
    let page = |dict: &str| {
        let doc = text_page(dict, "/Flags 32", &font, "FontFile2", "BT /F1 100 Tf 0 0 Td (A) Tj ET");
        let (b, w) = draw(&doc);
        assert!(w.is_empty(), "{w:?}");
        b.unwrap()
    };
    let tricky = page(TRICKY_TT);
    assert_eq!(pixel(&tricky, 2, 80), [0, 0, 0]);
    assert_eq!(pixel(&tricky, 20, 80), WHITE);
    // The same font under a name that is not on the list is drawn as its outlines are (39 pixels wide).
    let plain = page("/Subtype /TrueType /BaseFont /Test /FirstChar 65 /LastChar 66 /Widths [1000 1000] /Encoding /WinAnsiEncoding");
    assert_eq!(pixel(&plain, 2, 80), [0, 0, 0]);
    assert_eq!(pixel(&plain, 20, 80), [0, 0, 0]);
}

#[test]
fn a_tricky_font_whose_instructions_fail_is_drawn_without_them_and_warns_once() {
    let endless = ins_cat(&[&ins_words(&[-4]), &[0x1C]]);
    let glyphs = vec![tt_rect(0, 0, 0, 0), tt_glyph_ins(&[&SQUARE_800], &endless), tt_glyph_ins(&[&SQUARE_800], &[0x60]), tt_rect(0, 0, 0, 0)];
    let font = tricky_font(&glyphs, &[], &[], None);
    let doc = text_page(TRICKY_TT, "/Flags 32", &font, "FontFile2", "BT /F1 100 Tf 0 0 Td (ABAB) Tj ET");
    let (b, w) = draw(&doc);
    let b = b.unwrap();
    // A is the square, 39 pixels wide: the plain outline.
    assert_eq!(pixel(&b, 20, 80), [0, 0, 0]);
    let said: Vec<_> = w.iter().filter(|m| m.contains("instructions")).collect();
    assert_eq!(said.len(), 1, "{w:?}");
}

#[test]
fn glyphs_that_each_run_their_instructions_to_the_limit_are_cut_off_by_the_pages_budget() {
    // 2000 different glyphs, each with a program that runs until it is cut off (200,000 instructions): about a hundred of them
    // get that far before the page's work (20 million) is used up; the page is drawn all the same.
    let endless = ins_cat(&[&ins_words(&[-4]), &[0x1C]]);
    let glyphs: Vec<Vec<u8>> = (0..2001).map(|g| if g == 0 { tt_rect(0, 0, 0, 0) } else { tt_glyph_ins(&[&SQUARE_800], &endless) }).collect();
    let font = tricky_font(&glyphs, &[], &[], None);
    let codes: String = (1..=2000u32).map(|g| format!("{g:04X}")).collect();
    let font_dict = "/Subtype /Type0 /BaseFont /ABCDEF+DFKaiShu-SB /Encoding /Identity-H /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ABCDEF+DFKaiShu-SB /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 /CIDToGIDMap /Identity /FontDescriptor 6 0 R >>]";
    let doc = text_page(font_dict, "/Flags 4", &font, "FontFile2", &format!("BT /F1 4 Tf 0 90 Td <{codes}> Tj ET"));
    let started = Instant::now();
    let (b, w) = draw(&doc);
    assert!(b.is_ok());
    assert!(w.iter().any(|m| m.contains("took more work than a page may spend")), "{w:?}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

/// A table of a given length whose checksum (the sum of its 32-bit words) is `sum`.
fn forged_table(len: usize, sum: u32) -> Vec<u8> {
    let mut t = vec![0u8; len];
    t[..4].copy_from_slice(&sum.to_be_bytes());
    t
}

#[test]
fn a_font_is_tricky_by_the_checksums_of_its_tables_even_without_a_name_or_a_pdf_name() {
    // The tables of DFKaiShu as FreeType lists them (the programs are only filled to length: this test is about the finding).
    let (cvt, fpgm, prep) = (forged_table(0x350, 0x11E5_EAD4), forged_table(0x9063, 0x5A30_CA3B), forged_table(0x7E, 0x13A4_2602));
    let font = tricky_font_tables(&narrowing_glyphs(), &cvt, &fpgm, &prep, None);
    assert!(load_tt(font.clone()).unwrap().is_tricky());
    // A system font is a file: the same finding, and the tables are kept to run.
    let path = std::env::temp_dir().join(format!("qingpdf-tricky-{}.ttf", std::process::id()));
    std::fs::write(&path, &font).unwrap();
    let from_file = TtFont::from_file(&path, 0);
    let plain = tricky_font_tables(&narrowing_glyphs(), &cvt, &fpgm[..0x9062], &prep, Some("DFKai-SB"));
    std::fs::write(&path, &plain).unwrap();
    let not_from_file = TtFont::from_file(&path, 0);
    let _ = std::fs::remove_file(&path);
    assert!(from_file.unwrap().is_tricky());
    // One byte short in a table, and no tricky checksum: a system font is judged by its tables alone.
    assert!(!not_from_file.unwrap().is_tricky());
    assert!(load_tt(plain).unwrap().is_tricky(), "by its name table, as an embedded font");
}
