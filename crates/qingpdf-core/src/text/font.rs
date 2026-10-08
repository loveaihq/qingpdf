//! Fonts as far as text needs them (ISO 32000-1 9.5 to 9.8, 9.10): how a string
//! splits into character codes, what Unicode each code stands for, and how far
//! each one advances. Nothing here looks at glyph outlines.
//!
//! Where the Unicode value of a code comes from, in order (9.10.2): the font's
//! `/ToUnicode`; for a simple font, the name its encoding gives the code (the
//! Adobe Glyph List); for a CID font with a known Adobe collection, the
//! collection's CID to Unicode table; for a Unicode-coded predefined CMap, the
//! code itself.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::document::Document;
use crate::object::{Dict, ObjRef, Object};

use super::cmap::{self, CMap, MAX_USECMAP_DEPTH, ToUni, Uni};
use super::data::{self, Ordering};
use super::fontprog::{self, Kind};

/// Distinct warnings kept for one run; more are counted and dropped.
const MAX_WARNINGS: usize = 50;

#[derive(Default)]
pub(crate) struct Warnings {
    pub list: Vec<String>,
    seen: HashSet<String>,
}

impl Warnings {
    pub fn add(&mut self, message: impl Into<String>) {
        let message = message.into();
        if self.list.len() < MAX_WARNINGS && self.seen.insert(message.clone()) {
            self.list.push(message);
        }
    }
}

/// One character of a shown string.
pub(crate) struct Shown {
    /// The character code (a byte for a simple font; the code of the CMap for a composite one).
    pub code: u32,
    pub uni: Uni,
    /// Horizontal advance in text space units (width / 1000, or width times the
    /// font matrix for Type 3).
    pub w0: f64,
    /// Vertical writing: (w1, vx, vy) in text space units: the vertical advance
    /// and the position vector (9.7.4.3).
    pub vertical: Option<(f64, f64, f64)>,
    /// The code is the single byte 32: word spacing applies (9.3.3).
    pub is_space_code: bool,
}

pub(crate) struct Font {
    /// For warnings: the base font name.
    pub name: String,
    pub vertical: bool,
    body: Body,
}

enum Body {
    Simple(Simple),
    Composite(Composite),
}

struct Simple {
    /// The character of each code, packed to keep the table small (a page may switch between
    /// hundreds of fonts, and each lookup then touches a table that is not in the cache): 0 is
    /// none, a code point is that character, 0x8000_0000 + n is the n-th of `multi`.
    uni: Vec<u32>,
    multi: Vec<Rc<str>>,
    /// Width of each code in glyph space units (the font's own, else the missing width).
    widths: Vec<f32>,
    width_scale: f64,
}

const MULTI_FLAG: u32 = 0x8000_0000;

struct CachedChar {
    uni: Uni,
    w0: f64,
    vertical: Option<(f64, f64, f64)>,
}

struct Composite {
    cmap: Arc<CMap>,
    to_uni: Option<ToUni>,
    ordering: Option<Ordering>,
    /// `/Ordering (UCS)`: the CID is the Unicode value.
    cid_is_unicode: bool,
    dw: f64,
    w: Vec<(u32, u32, f64)>,
    dw2: (f64, f64),
    w2: Vec<(u32, u32, [f64; 3])>,
    cache: RefCell<HashMap<u64, Rc<CachedChar>>>,
}

// --- small helpers for reading dictionaries ---------------------------------------------------------

fn resolved(doc: &Document, obj: Option<&Object>) -> Option<Object> {
    match obj? {
        Object::Null => None,
        o => doc.resolve(o).ok().filter(|o| !matches!(o, Object::Null)),
    }
}

fn dict_at(doc: &Document, dict: &Dict, key: &str) -> Option<Dict> {
    match resolved(doc, dict.get(key))? {
        Object::Dict(d) => Some(d),
        Object::Stream(s) => Some(s.dict),
        _ => None,
    }
}

fn number_at(doc: &Document, obj: &Object) -> Option<f64> {
    resolved(doc, Some(obj))?.as_f64()
}

fn strip_subset_prefix(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

/// The built-in encoding of the font program of a simple font (9.6.6): `/FontFile` (Type 1),
/// `/FontFile3` (CFF), and `/FontFile2` (TrueType) when the font is symbolic (9.6.6.4; a
/// non-symbolic one is read through the names of its encoding). A program that cannot be read
/// leaves the font with the encoding it would have had, and one warning.
fn load_builtin_encoding(doc: &Document, descriptor: &Dict, flags: i64, name: &str, warnings: &mut Warnings) -> Option<fontprog::Table> {
    let (kind, key) = if descriptor.get("FontFile").is_some() {
        (Kind::Type1, "FontFile")
    } else if descriptor.get("FontFile3").is_some() {
        (Kind::Cff, "FontFile3")
    } else if descriptor.get("FontFile2").is_some() && flags & 4 != 0 {
        (Kind::TrueType, "FontFile2")
    } else {
        return None;
    };
    let program = stream_data(doc, descriptor.get(key), "font program", warnings)?;
    match fontprog::builtin_encoding(kind, &program) {
        Ok(table) => Some(table),
        Err(why) => {
            warnings.add(format!("font {name}: the built-in encoding of the font program was not read ({why})"));
            None
        }
    }
}

/// Decode a stream object's data, or say why not.
fn stream_data(doc: &Document, obj: Option<&Object>, what: &str, warnings: &mut Warnings) -> Option<Vec<u8>> {
    let Object::Stream(s) = resolved(doc, obj)? else { return None };
    match doc.decode_stream(&s) {
        Ok(d) => Some(d),
        Err(e) => {
            warnings.add(format!("{what} could not be read: {e}"));
            None
        }
    }
}

// --- loading -----------------------------------------------------------------------------------------

impl Font {
    pub fn load(doc: &Document, dict: &Dict, cmaps: &mut HashMap<String, Option<Arc<CMap>>>, warnings: &mut Warnings) -> Font {
        let subtype = dict.get_name("Subtype").map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned()).unwrap_or_default();
        let base = dict
            .get_name("BaseFont")
            .map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())
            .unwrap_or_default();
        let name = strip_subset_prefix(&base).to_string();
        let to_uni = load_to_unicode(doc, dict, warnings);
        if subtype == "Type0" {
            return Font::load_composite(doc, dict, name, to_uni, cmaps, warnings);
        }
        Font::load_simple(doc, dict, &subtype, name, to_uni, warnings)
    }

    fn load_simple(doc: &Document, dict: &Dict, subtype: &str, name: String, to_uni: Option<ToUni>, warnings: &mut Warnings) -> Font {
        if !matches!(subtype, "Type1" | "MMType1" | "TrueType" | "Type3") {
            warnings.add(format!("font {name}: unknown font type /{subtype}, read as a simple font"));
        }
        let descriptor = dict_at(doc, dict, "FontDescriptor");
        let flags = descriptor.as_ref().and_then(|d| d.get_int("Flags")).unwrap_or(0);
        let encoding = resolved(doc, dict.get("Encoding"));
        let (base_name, differences) = match &encoding {
            Some(Object::Name(n)) => (Some(String::from_utf8_lossy(n.as_bytes()).into_owned()), None),
            Some(Object::Dict(d)) => (
                d.get_name("BaseEncoding").map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned()),
                resolved(doc, d.get("Differences")),
            ),
            _ => (None, None),
        };
        let symbolic_name = name.starts_with("Symbol");
        let zapf_name = name.starts_with("ZapfDingbats");
        let table: &[u16; 256] = match base_name.as_deref() {
            Some("WinAnsiEncoding") => &data::WIN_ANSI,
            Some("MacRomanEncoding") => &data::MAC_ROMAN,
            Some("StandardEncoding") => &data::STANDARD,
            _ if zapf_name => &data::ZAPF_DINGBATS,
            _ if symbolic_name => &data::SYMBOL,
            // 9.6.6.4: a TrueType font without an encoding is read through its own cmap, which
            // we do not read; the Windows Latin set is the best guess. Type 1 and Type 3: the
            // Standard set (the glyph names of /Differences carry the rest).
            _ if subtype == "TrueType" && flags & 4 == 0 => &data::WIN_ANSI,
            _ => &data::STANDARD,
        };
        // The glyph names /Differences gives to codes (9.6.6.1): (code, name).
        let mut renamed: Vec<Option<Vec<u8>>> = vec![None; 256];
        if let Some(Object::Array(items)) = &differences {
            let mut code: Option<usize> = None;
            for item in items {
                match item {
                    Object::Integer(n) => code = usize::try_from(*n).ok(),
                    Object::Name(n) => {
                        if let Some(c) = code {
                            if let Some(slot) = renamed.get_mut(c) {
                                *slot = Some(n.as_bytes().to_vec());
                            }
                            code = Some(c + 1);
                        }
                    }
                    Object::Ref(_) => match resolved(doc, Some(item)) {
                        Some(Object::Integer(n)) => code = usize::try_from(n).ok(),
                        Some(Object::Name(n)) => {
                            if let Some(c) = code {
                                if let Some(slot) = renamed.get_mut(c) {
                                    *slot = Some(n.as_bytes().to_vec());
                                }
                                code = Some(c + 1);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        // 9.6.6.1, 9.6.6.2: without a /BaseEncoding the base is the font program's own encoding.
        let builtin = match (&descriptor, base_name.is_none() && !symbolic_name && !zapf_name) {
            (Some(d), true) => load_builtin_encoding(doc, d, flags, &name, warnings),
            _ => None,
        };
        // Per code: the font's ToUnicode first, then the name /Differences gave it, then the
        // font program's own encoding, then the base encoding. (Each is only looked up when the
        // ones before it do not know the code.)
        let uni: Vec<Uni> = (0..256usize)
            .map(|code| {
                if let Some(tu) = &to_uni {
                    let u = tu.get(u32::try_from(code).unwrap_or(0));
                    if !u.is_none() {
                        return u;
                    }
                }
                if let Some(Some(name)) = renamed.get(code) {
                    return fontprog::uni_of_glyph_name(&String::from_utf8_lossy(name));
                }
                // A glyph the program has but cannot give a character (a name that is no Unicode
                // name, say) leaves the code to the base encoding, as before.
                if let Some(Some(u)) = builtin.as_ref().and_then(|b| b.get(code))
                    && !u.is_none()
                {
                    return u.clone();
                }
                match table.get(code) {
                    Some(&cp) if cp != 0 => Uni::cp(u32::from(cp)),
                    _ => Uni::None,
                }
            })
            .collect();

        let first = dict.get_int("FirstChar").and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
        let mut own: Vec<Option<f64>> = vec![None; 256];
        let mut have_widths = false;
        if let Some(Object::Array(items)) = resolved(doc, dict.get("Widths")) {
            have_widths = true;
            for (i, item) in items.iter().enumerate().take(256) {
                if let Some(slot) = own.get_mut(first.saturating_add(i)) {
                    *slot = number_at(doc, item);
                }
            }
        }
        let missing_width = descriptor.as_ref().and_then(|d| d.get("MissingWidth")).and_then(Object::as_f64).unwrap_or(0.0);
        let is_type3 = subtype == "Type3";
        let font_matrix = match resolved(doc, dict.get("FontMatrix")) {
            Some(Object::Array(a)) if a.len() == 6 => {
                let mut m = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
                for (slot, v) in m.iter_mut().zip(&a) {
                    *slot = number_at(doc, v).filter(|v| v.is_finite()).unwrap_or(*slot);
                }
                m
            }
            _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
        };
        // 9.6.2.2: a font with /Widths gives codes it does not list the missing width. The 14
        // standard fonts may come without widths (deprecated); a rough guess keeps the spacing of
        // words sane: Courier is 600 wide, the others about 500 (a space 278).
        let widths: Vec<f64> = own
            .iter()
            .enumerate()
            .map(|(code, w)| match w {
                Some(w) => *w,
                None if have_widths => missing_width,
                None if name.starts_with("Courier") => 600.0,
                None if code == 32 => 278.0,
                None => 500.0,
            })
            .collect();
        let width_scale = if is_type3 { font_matrix[0] } else { 0.001 };
        let mut multi: Vec<Rc<str>> = Vec::new();
        let packed: Vec<u32> = uni
            .iter()
            .map(|u| match u {
                Uni::None => 0,
                Uni::One(c) => u32::from(*c),
                Uni::Many(text) => {
                    multi.push(text.clone());
                    MULTI_FLAG | u32::try_from(multi.len() - 1).unwrap_or(0)
                }
            })
            .collect();
        let widths = widths.iter().map(|&w| w as f32).collect();
        Font { name, vertical: false, body: Body::Simple(Simple { uni: packed, multi, widths, width_scale }) }
    }

    fn load_composite(
        doc: &Document,
        dict: &Dict,
        name: String,
        to_uni: Option<ToUni>,
        cmaps: &mut HashMap<String, Option<Arc<CMap>>>,
        warnings: &mut Warnings,
    ) -> Font {
        let descendant = match resolved(doc, dict.get("DescendantFonts")) {
            Some(Object::Array(a)) => a.first().and_then(|o| resolved(doc, Some(o))),
            Some(o @ Object::Dict(_)) => Some(o),
            _ => None,
        };
        let cid_font = descendant.as_ref().and_then(Object::as_dict).cloned().unwrap_or_else(|| {
            warnings.add(format!("font {name}: no descendant CID font"));
            Dict::new()
        });
        let ordering_name = dict_at(doc, &cid_font, "CIDSystemInfo")
            .and_then(|d| match resolved(doc, d.get("Ordering")) {
                Some(Object::String(s)) => Some(s.bytes),
                _ => None,
            })
            .unwrap_or_default();

        let cmap = match resolved(doc, dict.get("Encoding")) {
            Some(Object::Name(n)) => {
                let cmap_name = String::from_utf8_lossy(n.as_bytes()).into_owned();
                let found = cmaps.entry(cmap_name.clone()).or_insert_with(|| cmap::predefined(&cmap_name)).clone();
                found.unwrap_or_else(|| {
                    warnings.add(format!("font {name}: unknown predefined CMap {cmap_name}, read as Identity-H"));
                    Arc::new(CMap::identity(0))
                })
            }
            Some(Object::Stream(s)) => {
                let mut seen = Vec::new();
                if let Some(Object::Ref(r)) = dict.get("Encoding") {
                    seen.push(*r);
                }
                Arc::new(embedded_cmap(doc, &s, cmaps, &mut seen, 0, warnings))
            }
            _ => {
                warnings.add(format!("font {name}: no usable /Encoding, read as Identity-H"));
                Arc::new(CMap::identity(0))
            }
        };

        let mut w = Vec::new();
        if let Some(Object::Array(items)) = resolved(doc, cid_font.get("W")) {
            read_w(doc, &items, &mut w);
        }
        w.sort_by_key(|e| e.0);
        let mut w2 = Vec::new();
        if let Some(Object::Array(items)) = resolved(doc, cid_font.get("W2")) {
            read_w2(doc, &items, &mut w2);
        }
        w2.sort_by_key(|e| e.0);
        let dw = cid_font.get("DW").and_then(Object::as_f64).unwrap_or(1000.0);
        let dw2 = match resolved(doc, cid_font.get("DW2")) {
            Some(Object::Array(a)) if a.len() >= 2 => (
                a.first().and_then(Object::as_f64).unwrap_or(880.0),
                a.get(1).and_then(Object::as_f64).unwrap_or(-1000.0),
            ),
            _ => (880.0, -1000.0),
        };
        let ordering = Ordering::from_name(&ordering_name).or(cmap.ordering);
        let vertical = cmap.wmode == 1;
        Font {
            name,
            vertical,
            body: Body::Composite(Composite {
                cmap,
                to_uni,
                ordering,
                cid_is_unicode: ordering_name == b"UCS",
                dw,
                w,
                dw2,
                w2,
                cache: RefCell::new(HashMap::new()),
            }),
        }
    }
}

/// Entries of a `/W` array that are widths (9.7.4.3): `c [w1 w2 ...]` and `c_first c_last w`.
fn read_w(doc: &Document, items: &[Object], out: &mut Vec<(u32, u32, f64)>) {
    const MAX: usize = 2_000_000;
    let mut i = 0;
    while let Some(first) = items.get(i).and_then(|o| number_at(doc, o)) {
        if out.len() >= MAX {
            return;
        }
        let first = first.max(0.0) as u32;
        match items.get(i + 1).and_then(|o| resolved(doc, Some(o))) {
            Some(Object::Array(ws)) => {
                for (k, w) in ws.iter().enumerate() {
                    if let Some(w) = number_at(doc, w) {
                        let c = first.saturating_add(u32::try_from(k).unwrap_or(0));
                        out.push((c, c, w));
                    }
                }
                i += 2;
            }
            Some(last) => {
                let last = last.as_f64().unwrap_or(0.0).max(0.0) as u32;
                if let Some(w) = items.get(i + 2).and_then(|o| number_at(doc, o)) {
                    out.push((first, last.max(first), w));
                }
                i += 3;
            }
            None => return,
        }
    }
}

/// `/W2`: `c [w1y vx vy ...]` and `c_first c_last w1y vx vy`.
fn read_w2(doc: &Document, items: &[Object], out: &mut Vec<(u32, u32, [f64; 3])>) {
    const MAX: usize = 2_000_000;
    let mut i = 0;
    while let Some(first) = items.get(i).and_then(|o| number_at(doc, o)) {
        if out.len() >= MAX {
            return;
        }
        let first = first.max(0.0) as u32;
        match items.get(i + 1).and_then(|o| resolved(doc, Some(o))) {
            Some(Object::Array(ws)) => {
                let nums: Vec<f64> = ws.iter().filter_map(|w| number_at(doc, w)).collect();
                for (k, v) in nums.chunks(3).enumerate() {
                    if let [a, b, c] = v {
                        let cid = first.saturating_add(u32::try_from(k).unwrap_or(0));
                        out.push((cid, cid, [*a, *b, *c]));
                    }
                }
                i += 2;
            }
            Some(last) => {
                let last = last.as_f64().unwrap_or(0.0).max(0.0) as u32;
                let v: Vec<f64> = (2..5).filter_map(|k| items.get(i + k).and_then(|o| number_at(doc, o))).collect();
                if let [a, b, c] = v.as_slice() {
                    out.push((first, last.max(first), [*a, *b, *c]));
                }
                i += 5;
            }
            None => return,
        }
    }
}

fn load_to_unicode(doc: &Document, dict: &Dict, warnings: &mut Warnings) -> Option<ToUni> {
    let data = stream_data(doc, dict.get("ToUnicode"), "a /ToUnicode CMap", warnings)?;
    let parsed = cmap::parse(&data);
    if parsed.damaged {
        warnings.add("a /ToUnicode CMap is damaged; the part before the damage is used");
    }
    let table = ToUni::new(parsed.bf);
    (!table.is_empty()).then_some(table)
}

/// An embedded CMap stream, with its `/UseCMap` chain (cycles and chains deeper than
/// [`MAX_USECMAP_DEPTH`] are cut).
fn embedded_cmap(
    doc: &Document,
    stream: &crate::object::Stream,
    cmaps: &mut HashMap<String, Option<Arc<CMap>>>,
    seen: &mut Vec<ObjRef>,
    depth: usize,
    warnings: &mut Warnings,
) -> CMap {
    let data = match doc.decode_stream(stream) {
        Ok(d) => d,
        Err(e) => {
            warnings.add(format!("an embedded CMap could not be read: {e}"));
            Vec::new()
        }
    };
    let parsed = cmap::parse(&data);
    if parsed.damaged {
        warnings.add("an embedded CMap is damaged; the part before the damage is used");
    }
    let wmode = stream
        .dict
        .get_int("WMode")
        .map(|v| u8::from(v == 1))
        .or(parsed.wmode)
        .unwrap_or(0);
    let name = stream
        .dict
        .get_name("CMapName")
        .map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())
        .unwrap_or_default();
    let mut base: Option<Arc<CMap>> = None;
    if depth < MAX_USECMAP_DEPTH {
        match stream.dict.get("UseCMap") {
            Some(Object::Name(n)) => {
                let base_name = String::from_utf8_lossy(n.as_bytes()).into_owned();
                base = cmaps.entry(base_name.clone()).or_insert_with(|| cmap::predefined(&base_name)).clone();
            }
            Some(obj) => {
                let r = obj.as_obj_ref();
                if r.is_some_and(|r| seen.contains(&r)) {
                    warnings.add("an embedded CMap's /UseCMap chain loops; the loop is cut");
                } else if let Some(Object::Stream(s)) = resolved(doc, Some(obj)) {
                    seen.extend(r);
                    base = Some(Arc::new(embedded_cmap(doc, &s, cmaps, seen, depth + 1, warnings)));
                }
            }
            None => {
                if let Some(n) = &parsed.use_name {
                    let base_name = String::from_utf8_lossy(n).into_owned();
                    base = cmaps.entry(base_name.clone()).or_insert_with(|| cmap::predefined(&base_name)).clone();
                }
            }
        }
    }
    parsed.into_cmap(name, wmode, base)
}

// --- showing -----------------------------------------------------------------------------------------

fn lookup_w(table: &[(u32, u32, f64)], cid: u32) -> Option<f64> {
    let at = table.partition_point(|e| e.0 <= cid);
    // Ranges may overlap a little; the nearest ones before are checked.
    table.get(..at)?.iter().rev().take(4).find(|e| e.0 <= cid && cid <= e.1).map(|e| e.2)
}

impl Composite {
    fn unicode(&self, code: u32, cid: u32) -> Uni {
        if let Some(tu) = &self.to_uni {
            let u = tu.get(code);
            if !u.is_none() {
                return u;
            }
        }
        if self.cmap.codes_are_unicode() {
            return match self.cmap.kind {
                cmap::Kind::Utf16 if code > 0xFFFF => {
                    let units = [(code >> 16) as u16, code as u16];
                    Uni::from_utf16(&units)
                }
                _ => Uni::cp(code),
            };
        }
        if self.cid_is_unicode {
            return Uni::cp(cid);
        }
        match self.ordering.and_then(|o| o.cid_to_unicode(cid)) {
            Some(cp) => Uni::cp(cp),
            None => Uni::None,
        }
    }

    fn char_at(&self, code: u32, n: usize) -> Rc<CachedChar> {
        let key = u64::from(code) | ((n as u64) << 32);
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        let cid = if self.cmap.codes_are_unicode() {
            // The widths are by CID: find the CID of the character in the collection.
            let cp = if self.cmap.kind == cmap::Kind::Utf16 && code > 0xFFFF {
                char::decode_utf16([(code >> 16) as u16, code as u16]).next().and_then(Result::ok).map_or(0, u32::from)
            } else {
                code
            };
            if self.w.is_empty() && self.w2.is_empty() {
                0
            } else {
                self.ordering.and_then(|o| o.unicode_to_cid(cp)).unwrap_or(0)
            }
        } else {
            self.cmap.cid(code, n).unwrap_or(0)
        };
        let w0 = lookup_w(&self.w, cid).unwrap_or(self.dw) / 1000.0;
        let vertical = (self.cmap.wmode == 1).then(|| {
            let at = self.w2.partition_point(|e| e.0 <= cid);
            let entry = self.w2.get(..at).and_then(|s| s.iter().rev().take(4).find(|e| e.0 <= cid && cid <= e.1));
            match entry {
                Some(e) => (e.2[0] / 1000.0, e.2[1] / 1000.0, e.2[2] / 1000.0),
                // 9.7.4.3: default position vector (w0/2, DW2[0]), displacement DW2[1].
                None => (self.dw2.1 / 1000.0, w0 / 2.0, self.dw2.0 / 1000.0),
            }
        });
        let value = Rc::new(CachedChar { uni: self.unicode(code, cid), w0, vertical });
        let mut cache = self.cache.borrow_mut();
        if cache.len() < 200_000 {
            cache.insert(key, value.clone());
        }
        value
    }
}

impl Font {
    /// Split a shown string into characters and call `f` for each.
    pub fn show(&self, s: &[u8], mut f: impl FnMut(Shown)) {
        match &self.body {
            Body::Simple(simple) => {
                for &b in s {
                    let code = usize::from(b);
                    let packed = simple.uni.get(code).copied().unwrap_or(0);
                    let uni = if packed == 0 {
                        Uni::None
                    } else if packed & MULTI_FLAG == 0 {
                        char::from_u32(packed).map_or(Uni::None, Uni::One)
                    } else {
                        simple.multi.get((packed & !MULTI_FLAG) as usize).map_or(Uni::None, |t| Uni::Many(t.clone()))
                    };
                    f(Shown {
                        code: u32::from(b),
                        uni,
                        w0: f64::from(simple.widths.get(code).copied().unwrap_or(0.0)) * simple.width_scale,
                        vertical: None,
                        is_space_code: b == 32,
                    });
                }
            }
            Body::Composite(comp) => {
                let mut rest = s;
                while !rest.is_empty() {
                    let (code, n) = comp.cmap.next_code(rest);
                    let n = n.max(1).min(rest.len());
                    let c = comp.char_at(code, n);
                    f(Shown { code, uni: c.uni.clone(), w0: c.w0, vertical: c.vertical, is_space_code: n == 1 && code == 32 });
                    rest = rest.get(n..).unwrap_or(&[]);
                }
            }
        }
    }
}
