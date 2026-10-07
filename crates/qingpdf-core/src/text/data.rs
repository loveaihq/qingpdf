//! Tables compiled into the program, read from the blobs that
//! `data/gen_text_data.py` writes (zlib-compressed, decoded with miniz_oxide
//! the first time they are needed). Formats are described in that script.
//!
//! Sources: Adobe's cmap-resources and agl-aglfn (BSD-3-Clause, see
//! `third_party/`); the simple-font encodings are ISO 32000-1 Annex D.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use miniz_oxide::inflate::decompress_to_vec_zlib_with_limit;

use super::cmap::{CMap, CidRange, CodeSpace, Kind};

pub(crate) use super::encodings::{MAC_ROMAN, STANDARD, SYMBOL, WIN_ANSI, ZAPF_DINGBATS};

fn inflate(blob: &[u8], limit: usize) -> Vec<u8> {
    decompress_to_vec_zlib_with_limit(blob, limit).unwrap_or_default()
}

/// The Adobe character collections (9.7.3 `/Ordering`) we have tables for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum Ordering {
    Gb1,
    Cns1,
    Japan1,
    Korea1,
}

const ORDERINGS: [Ordering; 4] = [Ordering::Gb1, Ordering::Cns1, Ordering::Japan1, Ordering::Korea1];

static CID_TO_UNI_BLOBS: [&[u8]; 4] = [
    include_bytes!("../../data/text/cid2uni-gb1.bin"),
    include_bytes!("../../data/text/cid2uni-cns1.bin"),
    include_bytes!("../../data/text/cid2uni-japan1.bin"),
    include_bytes!("../../data/text/cid2uni-korea1.bin"),
];
static CMAP_BLOBS: [&[u8]; 4] = [
    include_bytes!("../../data/text/cmaps-gb1.bin"),
    include_bytes!("../../data/text/cmaps-cns1.bin"),
    include_bytes!("../../data/text/cmaps-japan1.bin"),
    include_bytes!("../../data/text/cmaps-korea1.bin"),
];
static AGL_BLOB: &[u8] = include_bytes!("../../data/text/agl.bin");
static CJK_NORM_BLOB: &[u8] = include_bytes!("../../data/text/cjknorm.bin");
static CJK_NORM: OnceLock<Vec<(u32, u32)>> = OnceLock::new();

static CID_TO_UNI: [OnceLock<Vec<u32>>; 4] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];
static UNI_TO_CID: [OnceLock<HashMap<u32, u32>>; 4] =
    [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];
static CMAP_SETS: [OnceLock<Vec<Arc<CMap>>>; 4] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];
static AGL: OnceLock<Agl> = OnceLock::new();

impl Ordering {
    /// From a CIDSystemInfo `/Ordering` string. `None` for `Identity`, `UCS`
    /// and anything else we have no table for.
    pub fn from_name(name: &[u8]) -> Option<Ordering> {
        match name {
            b"GB1" => Some(Ordering::Gb1),
            b"CNS1" => Some(Ordering::Cns1),
            b"Japan1" => Some(Ordering::Japan1),
            b"Korea1" => Some(Ordering::Korea1),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Ordering::Gb1 => 0,
            Ordering::Cns1 => 1,
            Ordering::Japan1 => 2,
            Ordering::Korea1 => 3,
        }
    }

    fn cid_table(self) -> &'static [u32] {
        let i = self.index();
        match CID_TO_UNI.get(i) {
            Some(cell) => cell.get_or_init(|| decode_cid_table(CID_TO_UNI_BLOBS.get(i).copied().unwrap_or(&[]))),
            None => &[],
        }
    }

    /// The Unicode value Adobe's collection gives a CID (9.10.2: "CIDs of an
    /// Adobe character collection map to Unicode through the collection's
    /// UCS2 CMap").
    pub fn cid_to_unicode(self, cid: u32) -> Option<u32> {
        let cp = *self.cid_table().get(usize::try_from(cid).ok()?)?;
        (cp != 0).then_some(cp)
    }

    /// The CID of a Unicode value (the lowest CID that maps to it), for the
    /// widths of fonts whose CMap is Unicode-coded.
    pub fn unicode_to_cid(self, cp: u32) -> Option<u32> {
        let i = self.index();
        let map = UNI_TO_CID.get(i)?.get_or_init(|| {
            let mut map = HashMap::new();
            for (cid, &cp) in self.cid_table().iter().enumerate() {
                if cp != 0 {
                    map.entry(cp).or_insert(u32::try_from(cid).unwrap_or(0));
                }
            }
            map
        });
        map.get(&cp).copied()
    }

    /// The legacy predefined CMap of this collection with that name (`-H` names only).
    pub fn predefined(self, name: &str) -> Option<Arc<CMap>> {
        let i = self.index();
        let set = CMAP_SETS.get(i)?.get_or_init(|| decode_cmaps(CMAP_BLOBS.get(i).copied().unwrap_or(&[]), self));
        set.iter().find(|c| c.name == name).cloned()
    }
}

/// The collection that has a legacy predefined CMap of this name.
pub(crate) fn find_predefined(name: &str) -> Option<Arc<CMap>> {
    ORDERINGS.iter().find_map(|o| o.predefined(name))
}

// --- blob reading ---------------------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn varint(&mut self) -> Option<u32> {
        let mut value: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            value |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return u32::try_from(value).ok();
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn string(&mut self) -> Option<String> {
        let n = self.varint()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn unzigzag(v: u32) -> i64 {
    let v = i64::from(v);
    if v & 1 == 0 { v >> 1 } else { -((v + 1) >> 1) }
}

fn decode_cid_table(blob: &[u8]) -> Vec<u32> {
    let data = inflate(blob, 4 << 20);
    let mut r = Reader { data: &data, pos: 0 };
    let Some(n) = r.varint() else { return Vec::new() };
    let mut table = Vec::with_capacity((n as usize).min(1 << 20));
    let mut prev: i64 = 0;
    for _ in 0..n {
        let Some(v) = r.varint() else { break };
        if v == 0 {
            table.push(0);
        } else {
            prev = prev + 1 + unzigzag(v - 1);
            table.push(u32::try_from(prev).unwrap_or(0));
        }
    }
    table
}

fn decode_cmaps(blob: &[u8], ordering: Ordering) -> Vec<Arc<CMap>> {
    let data = inflate(blob, 16 << 20);
    let mut r = Reader { data: &data, pos: 0 };
    let mut out: Vec<Arc<CMap>> = Vec::new();
    let Some(count) = r.varint() else { return out };
    for _ in 0..count {
        let Some(cmap) = decode_one_cmap(&mut r, &out, ordering) else { break };
        out.push(Arc::new(cmap));
    }
    out
}

fn decode_one_cmap(r: &mut Reader<'_>, earlier: &[Arc<CMap>], ordering: Ordering) -> Option<CMap> {
    let name = r.string()?;
    let flags = r.u8()?;
    let base = if flags & 1 != 0 {
        let use_name = r.string()?;
        earlier.iter().find(|c| c.name == use_name).cloned()
    } else {
        None
    };
    let nspaces = r.varint()?;
    let mut spaces = Vec::new();
    for _ in 0..nspaces {
        let n = r.u8()?;
        let lo = r.varint()?;
        let hi = r.varint()?;
        spaces.push(CodeSpace { n, lo, hi });
    }
    let mut ranges = Vec::new();
    for n in 1..=4u8 {
        let count = r.varint()?;
        let (mut prev_hi, mut prev_cid_end): (i64, i64) = (0, 0);
        for _ in 0..count {
            let lo = prev_hi + i64::from(r.varint()?);
            let hi = lo + i64::from(r.varint()?);
            let cid = prev_cid_end + 1 + unzigzag(r.varint()?);
            ranges.push(CidRange {
                n,
                lo: u32::try_from(lo).ok()?,
                hi: u32::try_from(hi).ok()?,
                cid: u32::try_from(cid).ok()?,
            });
            prev_hi = hi;
            prev_cid_end = cid + (hi - lo);
        }
    }
    Some(CMap { name, kind: Kind::Table, spaces, ranges, wmode: 0, base, ordering: Some(ordering) })
}

// --- CJK compatibility characters ------------------------------------------------------------

/// Kangxi radicals, CJK radicals and compatibility ideographs as the unified ideograph they
/// stand for (NFKC); every other code point is returned as it is.
pub(crate) fn normalize_cjk(cp: u32) -> u32 {
    if !matches!(cp, 0x2E80..=0x2FD5 | 0xF900..=0xFAD9 | 0x2F800..=0x2FA1D) {
        return cp;
    }
    let table = CJK_NORM.get_or_init(|| {
        let data = inflate(CJK_NORM_BLOB, 1 << 20);
        let mut r = Reader { data: &data, pos: 0 };
        let mut out = Vec::new();
        let (Some(n), mut prev) = (r.varint(), 0u32) else { return out };
        for _ in 0..n {
            let (Some(delta), Some(target)) = (r.varint(), r.varint()) else { break };
            prev = prev.saturating_add(delta);
            out.push((prev, target));
        }
        out
    });
    match table.binary_search_by_key(&cp, |&(from, _)| from) {
        Ok(i) => table.get(i).map_or(cp, |&(_, to)| to),
        Err(_) => cp,
    }
}

// --- Adobe Glyph List -----------------------------------------------------------------------

struct Agl {
    text: String,
    /// (name start, name end, value end) of every line, sorted by name.
    index: Vec<(usize, usize, usize)>,
}

fn agl() -> &'static Agl {
    AGL.get_or_init(|| {
        let text = String::from_utf8(inflate(AGL_BLOB, 4 << 20)).unwrap_or_default();
        let mut index = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let end = start + line.len();
            let body_end = if line.ends_with('\n') { end - 1 } else { end };
            if let Some(semi) = line.find(';') {
                index.push((start, start + semi, body_end));
            }
            start = end;
        }
        Agl { text, index }
    })
}

/// The Unicode values of a glyph name from the Adobe Glyph List (or ZapfDingbats).
fn agl_lookup(name: &str) -> Option<Vec<u32>> {
    let a = agl();
    let found = a
        .index
        .binary_search_by(|&(s, e, _)| a.text.get(s..e).unwrap_or("").cmp(name))
        .ok()?;
    let &(_, name_end, value_end) = a.index.get(found)?;
    let values = a.text.get(name_end + 1..value_end)?;
    let out: Vec<u32> = values.split(' ').filter_map(|h| u32::from_str_radix(h, 16).ok()).collect();
    (!out.is_empty()).then_some(out)
}

/// A glyph name as Unicode, by the rules of the Adobe Glyph List Specification:
/// drop everything from the first period, split at underscores (a ligature),
/// and read each component as a list name, `uniXXXX[XXXX...]` or `uXXXX[XX]`.
/// An empty result means the name says nothing about Unicode.
pub(crate) fn glyph_name_to_unicode(name: &str) -> Vec<u32> {
    let base = name.split('.').next().unwrap_or("");
    let mut out = Vec::new();
    for part in base.split('_') {
        if let Some(cps) = agl_lookup(part) {
            out.extend(cps);
        } else if let Some(hex) = part.strip_prefix("uni") {
            if hex.len() >= 4 && hex.len() % 4 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase()) {
                for chunk in hex.as_bytes().chunks(4) {
                    let s = std::str::from_utf8(chunk).unwrap_or("");
                    if let Ok(v) = u32::from_str_radix(s, 16) {
                        out.push(v);
                    }
                }
            }
        } else if let Some(hex) = part.strip_prefix('u')
            && (4..=6).contains(&hex.len())
            && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase())
            && let Ok(v) = u32::from_str_radix(hex, 16)
        {
            out.push(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_name_to_unicode("A"), vec![0x41]);
        assert_eq!(glyph_name_to_unicode("fi"), vec![0xFB01]);
        assert_eq!(glyph_name_to_unicode("f_f_i"), vec![0x66, 0x66, 0x69]);
        assert_eq!(glyph_name_to_unicode("uni4E2D"), vec![0x4E2D]);
        assert_eq!(glyph_name_to_unicode("u20AC"), vec![0x20AC]);
        assert_eq!(glyph_name_to_unicode("a.sc"), vec![0x61]);
        assert_eq!(glyph_name_to_unicode("a1"), vec![0x2701]);
        assert_eq!(glyph_name_to_unicode("Alpha"), vec![0x391]);
        assert!(glyph_name_to_unicode("g123").is_empty());
    }

    #[test]
    fn cjk_compatibility_characters_are_normalized() {
        assert_eq!(normalize_cjk(0x2F00), 0x4E00); // Kangxi radical one
        assert_eq!(normalize_cjk(0x2F08), 0x4EBA); // person
        assert_eq!(normalize_cjk(0x2EC5), 0x89C1); // CJK radical simplified see
        assert_eq!(normalize_cjk(0xF900), 0x8C48); // compatibility ideograph
        assert_eq!(normalize_cjk(0x4E00), 0x4E00);
        assert_eq!(normalize_cjk(0x41), 0x41);
    }

    #[test]
    fn cid_tables() {
        // Adobe-GB1: CID 1 is the space, CID 96 the ideographic space.
        assert_eq!(Ordering::Gb1.cid_to_unicode(1), Some(0x20));
        assert_eq!(Ordering::Gb1.cid_to_unicode(96), Some(0x3000));
        assert_eq!(Ordering::Gb1.cid_to_unicode(0), None);
        assert_eq!(Ordering::Japan1.cid_to_unicode(843), Some(0x3042)); // hiragana a
        assert_eq!(Ordering::Gb1.unicode_to_cid(0x3000), Some(96));
    }

    #[test]
    fn predefined_cmaps() {
        let gbk = Ordering::Gb1.predefined("GBK-EUC-H").expect("GBK-EUC-H is embedded");
        assert_eq!(gbk.cid(0xA1A1, 2), Some(96));
        assert_eq!(gbk.cid(0x20, 1), Some(7716)); // half-width space
        assert_eq!(gbk.cid(0x41, 1), Some(846)); // half-width A
        assert_eq!(Ordering::Gb1.cid_to_unicode(846), Some(0x41));
        let ms = Ordering::Cns1.predefined("ETenms-B5-H").expect("ETenms-B5-H is embedded");
        assert!(ms.base.is_some());
        assert!(find_predefined("90ms-RKSJ-H").is_some());
        assert!(find_predefined("KSCms-UHC-H").is_some());
        assert!(find_predefined("Nonexistent-H").is_none());
    }
}
