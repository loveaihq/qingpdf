//! Which fonts are "tricky": fonts whose glyphs only come out right when their TrueType instructions are run
//! (the strokes of a Chinese character are separate components, put in place by the instructions).
//!
//! The list of names and the table of checksums are data: the names (family or PostScript name, without the
//! `ABCDEF+` tag of a subset) and the (checksum, length) of the `cvt `, `fpgm` and `prep` tables of the known fonts
//! (the same ones FreeType lists; the sum is that of the table read as big-endian 32-bit words, zero-padded). A PDF
//! subsetter often renames a font or drops its `name` table, so the tables are the sure test.

/// A name that contains one of these belongs to a tricky family.
const NAMES: [&str; 20] = [
    "cpop",
    "DFGirl-W6-WIN-BF",
    "DFGothic-EB",
    "DFGyoSho-Lt",
    "DFHei",
    "DFHSGothic-W5",
    "DFHSMincho-W3",
    "DFHSMincho-W7",
    "DFKaiSho-SB",
    "DFKaiShu",
    "DFKai-SB",
    "DFMing",
    "DLC",
    "HuaTianKaiTi",
    "HuaTianSongTi",
    "Ming(for ISO10646)",
    "MingLiU",
    "MingMedium",
    "PMingLiU",
    "MingLi43",
];

/// Per font: (checksum, length) of `cvt `, `fpgm`, `prep`. A length of 0 means the font has no such table.
#[rustfmt::skip]
const TABLES: [[(u32, usize); 3]; 31] = [
    [(0x05BCF058, 0x000002E4), (0x28233BF1, 0x000087C4), (0xA344A1EA, 0x000001E1)], // MingLiU 1995
    [(0x05BCF058, 0x000002E4), (0x28233BF1, 0x000087C4), (0xA344A1EB, 0x000001E1)], // MingLiU 1996-
    [(0x12C3EBB2, 0x00000350), (0xB680EE64, 0x000087A7), (0xCE939563, 0x00000758)], // DFGothic-EB
    [(0x11E5EAD4, 0x00000350), (0xCE5956E9, 0x0000BC85), (0x8272F416, 0x00000045)], // DFGyoSho-Lt
    [(0x1257EB46, 0x00000350), (0xF699D160, 0x0000715F), (0xD222F568, 0x000003BC)], // DFHei-Md-HK-BF
    [(0x1262EB4E, 0x00000350), (0xE86A5D64, 0x00007940), (0x7850F729, 0x000005FF)], // DFHSGothic-W5
    [(0x122DEB0A, 0x00000350), (0x3D16328A, 0x0000859B), (0xA93FC33B, 0x000002CB)], // DFHSMincho-W3
    [(0x125FEB26, 0x00000350), (0xA5ACC982, 0x00007EE1), (0x90999196, 0x0000041F)], // DFHSMincho-W7
    [(0x11E5EAD4, 0x00000350), (0x5A30CA3B, 0x00009063), (0x13A42602, 0x0000007E)], // DFKaiShu
    [(0x11E5EAD4, 0x00000350), (0xA6E78C01, 0x00008998), (0x13A42602, 0x0000007E)], // DFKaiShu, variant
    [(0x11E5EAD4, 0x00000360), (0x9DB282B2, 0x0000C06E), (0x53E6D7CA, 0x00000082)], // DFKaiShu-Md-HK-BF
    [(0x1243EB18, 0x00000350), (0xBA0A8C30, 0x000074AD), (0xF3D83409, 0x0000037B)], // DFMing-Bd-HK-BF
    [(0x07DCF546, 0x00000308), (0x40FE7C90, 0x00008E2A), (0x608174B5, 0x0000007A)], // DLCLiShu
    [(0xEB891238, 0x00000308), (0xD2E4DCD4, 0x0000676F), (0x8EA5F293, 0x000003B8)], // DLCHayBold
    [(0xFFFBFFFC, 0x00000008), (0x9C9E48B8, 0x0000BEA2), (0x70020112, 0x00000008)], // HuaTianKaiTi
    [(0xFFFBFFFC, 0x00000008), (0x0A5A0483, 0x00017C39), (0x70020112, 0x00000008)], // HuaTianSongTi
    [(0x00000000, 0x00000000), (0x40C92555, 0x000000E5), (0xA39B58E3, 0x0000117C)], // NEC fadpop7.ttf
    [(0x00000000, 0x00000000), (0x33C41652, 0x000000E5), (0x26D6C52A, 0x00000F6A)], // NEC fadrei5.ttf
    [(0x00000000, 0x00000000), (0x6DB1651D, 0x0000019D), (0x6C6E4B03, 0x00002492)], // NEC fangot7.ttf
    [(0x00000000, 0x00000000), (0x40C92555, 0x000000E5), (0xDE51FAD0, 0x0000117C)], // NEC fangyo5.ttf
    [(0x00000000, 0x00000000), (0x85E47664, 0x000000E5), (0xA6C62831, 0x00001CAA)], // NEC fankyo5.ttf
    [(0x00000000, 0x00000000), (0x2D891CFD, 0x0000019D), (0xA0604633, 0x00001DE8)], // NEC fanrgo5.ttf
    [(0x00000000, 0x00000000), (0x40AA774C, 0x000001CB), (0x9B5CAA96, 0x00001F9A)], // NEC fangot5.ttc
    [(0x00000000, 0x00000000), (0x0D3DE9CB, 0x00000141), (0xD4127766, 0x00002280)], // NEC fanmin3.ttc
    [(0x00000000, 0x00000000), (0x4A692698, 0x000001F0), (0x340D4346, 0x00001FCA)], // NEC FA-Gothic, 1996
    [(0x00000000, 0x00000000), (0xCD34C604, 0x00000166), (0x6CF31046, 0x000022B0)], // NEC FA-Minchou, 1996
    [(0x00000000, 0x00000000), (0x5DA75315, 0x0000019D), (0x40745A5F, 0x000022E0)], // NEC FA-RoundGothicB, 1996
    [(0x00000000, 0x00000000), (0xF055FC48, 0x000001C2), (0x3900DED3, 0x00001E18)], // NEC FA-RoundGothicM, 1996
    [(0x00170003, 0x00000060), (0xDBB4306E, 0x000058AA), (0xD643482A, 0x00000035)], // MINGLI.TTF, 1992
    [(0x1269EB58, 0x00000350), (0x5CD5957A, 0x00006A4E), (0xF758323A, 0x00000380)], // DFHei-Bd-WIN-HK-BF, issue #1087
    [(0x122FEB0B, 0x00000350), (0x7F10919A, 0x000070A9), (0x7CD7E7B7, 0x0000025C)], // DFMing-Md-WIN-HK-BF, issue #1087
];

/// The checksum of a table: the sum of its big-endian 32-bit words, the last one padded with zeros.
fn checksum(table: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in table.chunks(4) {
        let mut w = [0u8; 4];
        for (d, s) in w.iter_mut().zip(chunk) {
            *d = *s;
        }
        sum = sum.wrapping_add(u32::from_be_bytes(w));
    }
    sum
}

/// The name of a tricky font, with or without a subset tag.
pub(super) fn name_is_tricky(name: &str) -> bool {
    let b = name.as_bytes();
    let name = if b.len() > 7 && b.get(6) == Some(&b'+') && b.iter().take(6).all(u8::is_ascii_uppercase) { name.get(7..).unwrap_or(name) } else { name };
    NAMES.iter().any(|n| name.contains(n))
}

/// Do the three tables (an empty slice for a table the font lacks) belong to one of the known fonts?
pub(super) fn tables_are_tricky(cvt: &[u8], fpgm: &[u8], prep: &[u8]) -> bool {
    let tables = [cvt, fpgm, prep];
    // The length is compared first; the sums are only worked out for the tables that have a candidate.
    TABLES.iter().any(|face| {
        face.iter().zip(tables).all(|(&(sum, len), t)| if len == 0 { t.is_empty() } else { t.len() == len && checksum(t) == sum })
    })
}

/// Is one of the names in a `name` table (family, full, PostScript, typographic family) a tricky one?
pub(super) fn name_table_is_tricky(table: &[u8]) -> bool {
    let u16_at = |at: usize| crate::text::fontprog::u16_at(table, at).map(usize::from);
    let (Some(count), Some(storage)) = (u16_at(2), u16_at(4)) else { return false };
    for i in 0..count.min(512) {
        let rec = 6 + 12 * i;
        let (Some(platform), Some(id), Some(len), Some(off)) = (u16_at(rec), u16_at(rec + 6), u16_at(rec + 8), u16_at(rec + 10)) else { return false };
        if !matches!(id, 1 | 4 | 6 | 16) || len > 512 {
            continue;
        }
        let Some(bytes) = table.get(storage.saturating_add(off)..storage.saturating_add(off).saturating_add(len)) else { continue };
        let name: String = if platform == 1 {
            bytes.iter().map(|&b| char::from(b)).collect()
        } else {
            char::decode_utf16((0..bytes.len() / 2).filter_map(|k| crate::text::fontprog::u16_at(bytes, k * 2))).map(|c| c.unwrap_or('?')).collect()
        };
        if name_is_tricky(&name) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of a given length whose checksum is `sum`.
    fn forge(len: usize, sum: u32) -> Vec<u8> {
        let mut t = vec![0u8; len];
        t.get_mut(..4).unwrap().copy_from_slice(&sum.to_be_bytes());
        t
    }

    fn name_table(platform: u16, id: u16, name: &str) -> Vec<u8> {
        let text: Vec<u8> = if platform == 1 { name.bytes().collect() } else { name.encode_utf16().flat_map(u16::to_be_bytes).collect() };
        let mut t = vec![0, 0, 0, 1, 0, 18];
        for v in [platform, 1, 0x409, id, text.len() as u16, 0] {
            t.extend(v.to_be_bytes());
        }
        t.extend(text);
        t
    }

    #[test]
    fn names_of_the_known_fonts() {
        for name in ["DFKaiShu-SB-Estd-BF", "ABCDEF+DFKaiShu-SB-Estd-BF", "MingLiU", "XYZABC+PMingLiU", "DFHei-Bd-WIN-HK-BF", "DFKai-SB", "HuaTianSongTi"] {
            assert!(name_is_tricky(name), "{name}");
        }
        for name in ["Arial", "TimesNewRomanPSMT", "MicrosoftJhengHei", "SimSun", "", "ABCDEF+Arial"] {
            assert!(!name_is_tricky(name), "{name}");
        }
    }

    #[test]
    fn tables_of_the_known_fonts() {
        // DFKaiShu: cvt, fpgm and prep with the sums and lengths of the known font.
        let (cvt, fpgm, prep) = (forge(0x350, 0x11E5_EAD4), forge(0x9063, 0x5A30_CA3B), forge(0x7E, 0x13A4_2602));
        assert!(tables_are_tricky(&cvt, &fpgm, &prep));
        let mut other = fpgm.clone();
        other.truncate(0x9062);
        assert!(!tables_are_tricky(&cvt, &other, &prep));
        other = fpgm.clone();
        other[100] = 1;
        assert!(!tables_are_tricky(&cvt, &other, &prep));
        assert!(!tables_are_tricky(&cvt, &fpgm, &[]));
        // A font with no cvt table, like the NEC fonts in the list; and no tables at all.
        assert!(tables_are_tricky(&[], &forge(0xE5, 0x40C9_2555), &forge(0x117C, 0xA39B_58E3)));
        assert!(!tables_are_tricky(&[], &[], &[]));
        // The checksum pads the last word with zeros.
        assert_eq!(checksum(&[1, 2, 3, 4, 5]), 0x0102_0304u32.wrapping_add(0x0500_0000));
    }

    #[test]
    fn names_in_a_name_table() {
        assert!(name_table_is_tricky(&name_table(3, 1, "DFKai-SB")));
        assert!(name_table_is_tricky(&name_table(1, 6, "DFKaiShu-SB-Estd-BF")));
        assert!(!name_table_is_tricky(&name_table(3, 1, "Arial")));
        // Only the family, full and PostScript names count; a copyright notice that mentions a font does not.
        assert!(!name_table_is_tricky(&name_table(3, 0, "DFKai-SB")));
        // Damaged tables.
        let t = name_table(3, 1, "DFKai-SB");
        assert!(!name_table_is_tricky(&t[..10]));
        assert!(!name_table_is_tricky(&[]));
        let mut lie = t.clone();
        lie[4..6].copy_from_slice(&[0xFF, 0xFF]);
        assert!(!name_table_is_tricky(&lie));
    }
}
