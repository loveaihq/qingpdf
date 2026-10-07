//! What `qingpdf info` reports: version, pages and their sizes, the document
//! information dictionary, and how the file is built.

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::Object;
use crate::security::Encryption;

/// Points per millimetre's inverse: 1 pt = 1/72 in = 25.4/72 mm.
const MM_PER_PT: f64 = 25.4 / 72.0;

/// One page as shown to a reader.
#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    /// Width and height in points after rotation, as displayed (the crop box
    /// where the page has one, else the media box). `None` if the page has no
    /// usable box.
    pub size_pt: Option<(f64, f64)>,
    /// The `/Rotate` value: 0, 90, 180 or 270.
    pub rotation: i64,
}

impl PageInfo {
    /// The size in millimetres.
    pub fn size_mm(&self) -> Option<(f64, f64)> {
        self.size_pt.map(|(w, h)| (w * MM_PER_PT, h * MM_PER_PT))
    }
}

/// Everything `info` prints.
#[derive(Debug, Clone)]
pub struct Report {
    pub version: (u8, u8),
    pub pages: Vec<PageInfo>,
    /// Why the pages are not listed (the file is locked and its page tree is
    /// in an encrypted object stream), if that is so.
    pub pages_unavailable: Option<String>,
    pub encrypted: bool,
    /// Method, revision, permissions and which password opened the file.
    pub encryption: Option<Encryption>,
    pub xref_streams: bool,
    pub object_streams: bool,
    pub repaired: bool,
    /// Entries of the document information dictionary (14.3.3) whose values
    /// are text or names, with the text decoded. Empty when there is none.
    pub info: Vec<(String, String)>,
    /// Why the information dictionary is not shown (it is encrypted, or
    /// cannot be read), if that is so.
    pub info_unavailable: Option<String>,
}

/// Gather the report. Fails only if the page tree cannot be read (for a locked
/// file that is a note in the report, not a failure: `info` still says how the
/// file is protected).
pub fn describe(doc: &Document) -> Result<Report> {
    let (pages, pages_unavailable) = match doc.pages() {
        Ok(pages) => (pages.iter().map(page_info).collect(), None),
        Err(e) if doc.is_locked() => (Vec::new(), Some(e.to_string())),
        Err(e) => return Err(e),
    };
    let (info, info_unavailable) = match doc.info() {
        Ok(Some(dict)) => {
            let mut entries = Vec::new();
            for (key, value) in dict.iter() {
                let text = match doc.resolve(value) {
                    Ok(Object::String(s)) => decode_text_string(&s.bytes),
                    Ok(Object::Name(n)) => decode_text_string(n.as_bytes()),
                    _ => continue,
                };
                entries.push((decode_text_string(key.as_bytes()), text));
            }
            (entries, None)
        }
        Ok(None) => (Vec::new(), None),
        Err(Error::Unsupported(why)) => (Vec::new(), Some(why)),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    Ok(Report {
        version: doc.version(),
        pages,
        pages_unavailable,
        encrypted: doc.is_encrypted(),
        encryption: doc.encryption(),
        xref_streams: doc.uses_xref_streams(),
        object_streams: doc.uses_object_streams(),
        repaired: doc.was_repaired(),
        info,
        info_unavailable,
    })
}

/// The displayed size of a page: crop box clipped to the media box (14.11.2),
/// swapped when the page is rotated a quarter turn.
fn page_info(page: &Page) -> PageInfo {
    let rotation = page.rotate();
    let visible = page.media_box().map(|media| match page.crop_box() {
        Some(crop) => {
            let clipped = [crop[0].max(media[0]), crop[1].max(media[1]), crop[2].min(media[2]), crop[3].min(media[3])];
            if clipped[2] > clipped[0] && clipped[3] > clipped[1] { clipped } else { media }
        }
        None => media,
    });
    let size_pt = visible.map(|b| {
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        if rotation % 180 == 90 { (h, w) } else { (w, h) }
    });
    PageInfo { size_pt, rotation }
}

/// PDFDocEncoding (Annex D.3, Table D.2) above 0x7F and in 0x18-0x1F, as
/// Unicode. Everything else is the same as Latin-1.
pub(crate) fn pdf_doc_char(b: u8) -> char {
    const LOW: [char; 8] = ['\u{2D8}', '\u{2C7}', '\u{2C6}', '\u{2D9}', '\u{2DD}', '\u{2DB}', '\u{2DA}', '\u{2DC}'];
    const HIGH: [char; 31] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{192}', '\u{2044}', '\u{2039}',
        '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}',
        '\u{2122}', '\u{FB01}', '\u{FB02}', '\u{141}', '\u{152}', '\u{160}', '\u{178}', '\u{17D}', '\u{131}',
        '\u{142}', '\u{153}', '\u{161}', '\u{17E}',
    ];
    match b {
        0x18..=0x1F => LOW.get(usize::from(b - 0x18)).copied().unwrap_or('\u{FFFD}'),
        0x80..=0x9E => HIGH.get(usize::from(b - 0x80)).copied().unwrap_or('\u{FFFD}'),
        0xA0 => '\u{20AC}',
        // Undefined in PDFDocEncoding.
        0x7F | 0x9F | 0xAD => '\u{FFFD}',
        other => char::from(other),
    }
}

/// Decode a text string (ISO 32000-1 7.9.2.2): UTF-16BE when it starts with
/// the byte order mark FE FF, UTF-8 behind EF BB BF (PDF 2.0), otherwise
/// PDFDocEncoding. UTF-16LE (FF FE) is not allowed by the spec but seen in
/// the wild, so it is read too. Language escapes (U+001B ... U+001B) are
/// dropped. Control characters become spaces, so a hostile file cannot send
/// terminal escape codes through `info`.
pub fn decode_text_string(bytes: &[u8]) -> String {
    let decoded: String = if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        decode_utf16(rest, true)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        decode_utf16(rest, false)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| pdf_doc_char(b)).collect()
    };
    strip_language_escapes(&decoded)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> String {
    // A trailing odd byte is ignored.
    let (pairs, _) = bytes.as_chunks::<2>();
    let units = pairs.iter().map(|&pair| if big_endian { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) });
    char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

/// Remove `U+001B language [country] U+001B` sequences (7.9.2.2).
fn strip_language_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1B}' {
            // Skip to the closing escape; an unterminated one eats the rest,
            // which is as good a guess as any.
            for inner in chars.by_ref() {
                if inner == '\u{1B}' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    #[test]
    fn text_strings_in_all_three_encodings() {
        assert_eq!(decode_text_string(b"Plain ASCII"), "Plain ASCII");
        // PDFDocEncoding specials: bullet, Euro, ellipsis, circumflex accent.
        assert_eq!(decode_text_string(&[0x80, b' ', 0xA0, b' ', 0x83, b' ', 0x1A]), "\u{2022} \u{20AC} \u{2026} \u{2C6}");
        // Latin-1 range: e acute, u dieresis.
        assert_eq!(decode_text_string(&[b'c', b'a', b'f', 0xE9, b' ', 0xFC]), "caf\u{E9} \u{FC}");
        // Undefined code points do not crash.
        assert_eq!(decode_text_string(&[0x7F, 0x9F, 0xAD]), "\u{FFFD}\u{FFFD}\u{FFFD}");
        // UTF-16BE with the byte order mark, Chinese and a surrogate pair.
        let mut utf16 = vec![0xFE, 0xFF];
        for u in "\u{4E2D}\u{6587} \u{1F600}".encode_utf16() {
            utf16.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode_text_string(&utf16), "\u{4E2D}\u{6587} \u{1F600}");
        // A lone surrogate and an odd trailing byte are replaced, not fatal.
        assert_eq!(decode_text_string(&[0xFE, 0xFF, 0xD8, 0x00, 0x00, 0x41, 0x00]), "\u{FFFD}A");
        // UTF-16LE (not allowed, but seen) and UTF-8 with a BOM (PDF 2.0).
        assert_eq!(decode_text_string(&[0xFF, 0xFE, 0x2D, 0x4E]), "\u{4E2D}");
        assert_eq!(decode_text_string(&[0xEF, 0xBB, 0xBF, 0xE4, 0xB8, 0xAD]), "\u{4E2D}");
        assert_eq!(decode_text_string(b""), "");
    }

    #[test]
    fn language_escapes_and_control_characters_are_cleaned() {
        let mut s = vec![0xFE, 0xFF, 0x00, 0x1B];
        s.extend_from_slice(&[0x00, b'e', 0x00, b'n', 0x00, 0x1B, 0x00, b'H', 0x00, b'i']);
        assert_eq!(decode_text_string(&s), "Hi");
        assert_eq!(decode_text_string(b"a\x07b\nc\tz"), "a b c z");
        // In PDFDocEncoding 0x1B is the dot accent, not an escape.
        assert_eq!(decode_text_string(b"x\x1b[0m"), "x\u{2D9}[0m");
    }

    #[test]
    fn report_for_a_plain_file() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        let r = describe(&doc).unwrap();
        assert_eq!(r.version, (1, 4));
        assert_eq!(r.pages.len(), 1);
        assert_eq!(r.pages[0].size_pt, Some((200.0, 100.0)));
        let (w, h) = r.pages[0].size_mm().unwrap();
        assert!((w - 70.5556).abs() < 1e-3 && (h - 35.2778).abs() < 1e-3);
        assert_eq!(r.pages[0].rotation, 0);
        assert!(!r.encrypted && !r.xref_streams && !r.object_streams && !r.repaired);
        assert!(r.info.is_empty());
        assert_eq!(r.info_unavailable, None);
        let doc = Document::from_bytes(sample_objstm_pdf()).unwrap();
        let r = describe(&doc).unwrap();
        assert!(r.xref_streams && r.object_streams);
        assert_eq!(r.pages[0].size_pt, Some((300.0, 400.0)));
    }

    #[test]
    fn sizes_follow_crop_box_and_rotation() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 /MediaBox [0 0 600 400] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Rotate 90 >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /CropBox [100 100 400 300] >>");
        // A crop box outside the media box is ignored; none of it is visible.
        b.obj(5, "<< /Type /Page /Parent 2 0 R /CropBox [1000 1000 2000 2000] /Rotate 270 >>");
        b.obj(6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 0 0] >>");
        let doc = Document::from_bytes(b.finish_classic(7, "/Root 1 0 R")).unwrap();
        let r = describe(&doc).unwrap();
        let sizes: Vec<_> = r.pages.iter().map(|p| (p.size_pt, p.rotation)).collect();
        assert_eq!(
            sizes,
            vec![
                (Some((400.0, 600.0)), 90),
                (Some((300.0, 200.0)), 0),
                (Some((400.0, 600.0)), 270),
                (Some((0.0, 0.0)), 0),
            ]
        );
    }

    #[test]
    fn info_fields_are_decoded() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(
            3,
            "<< /Title <FEFF4E2D6587> /Author (Jane \\(J.\\) Doe) /Producer 4 0 R /Trapped /False /Pages 5 /Keywords (caf\\351) >>",
        );
        b.obj(4, "(qingpdf)");
        let doc = Document::from_bytes(b.finish_classic(5, "/Root 1 0 R /Info 3 0 R")).unwrap();
        let r = describe(&doc).unwrap();
        let get = |k: &str| r.info.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("Title"), Some("\u{4E2D}\u{6587}"));
        assert_eq!(get("Author"), Some("Jane (J.) Doe"));
        assert_eq!(get("Producer"), Some("qingpdf"));
        assert_eq!(get("Trapped"), Some("False"));
        assert_eq!(get("Keywords"), Some("caf\u{E9}"));
        // Numbers are not shown.
        assert_eq!(get("Pages"), None);
    }

    #[test]
    fn encrypted_file_reports_structure_but_not_info() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 20] >>");
        b.obj(4, &format!("<< /Filter /Standard /V 1 /R 2 /O <{}> /U <{}> /P -4 >>", "00".repeat(32), "11".repeat(32)));
        b.obj(5, "<< /Title (scrambled) >>");
        let doc = Document::from_bytes(b.finish_classic(6, "/Root 1 0 R /Encrypt 4 0 R /Info 5 0 R /ID [<aa> <bb>]")).unwrap();
        let r = describe(&doc).unwrap();
        assert!(r.encrypted);
        assert_eq!(r.pages.len(), 1);
        assert!(r.info.is_empty());
        assert!(r.info_unavailable.as_deref().is_some_and(|m| m.contains("needs a password")), "{:?}", r.info_unavailable);
        let encryption = r.encryption.expect("encryption is described");
        assert!(encryption.needs_password() && encryption.opened.is_none());
        assert_eq!((encryption.version, encryption.revision), (1, 2));
        assert_eq!(encryption.method_name(), "RC4 40-bit");
    }
}
