//! Text extraction (layer 2): the text of a page in reading order, with the
//! Unicode value of every character worked out from the fonts (ISO 32000-1
//! 9.10). Nothing is drawn.
//!
//! ```no_run
//! # use qingpdf_core::{Document, text};
//! let doc = Document::open("a.pdf")?;
//! let mut extractor = text::TextExtractor::new(&doc);
//! for page in doc.pages()? {
//!     print!("{}\u{c}", extractor.page_text(&page)?);
//! }
//! # Ok::<(), qingpdf_core::Error>(())
//! ```

pub(crate) mod cmap;
pub(crate) mod data;
mod encodings;
pub(crate) mod font;
pub(crate) mod fontprog;
pub(crate) mod interp;
mod layout;
pub(crate) mod scan;

use std::rc::Rc;

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Object, Stream};
use crate::ops::has_owner_rights;

use interp::{Interp, Scope, Shared};

/// Most decoded content stream bytes one page may have, all its streams together.
pub(crate) const MAX_PAGE_CONTENT: usize = 256 * 1024 * 1024;

/// Reads the text of the pages of one document. Fonts and CMaps are kept between
/// pages, so one extractor should do all the pages of a run.
pub struct TextExtractor<'a> {
    doc: &'a Document,
    shared: Shared,
}

impl<'a> TextExtractor<'a> {
    pub fn new(doc: &'a Document) -> TextExtractor<'a> {
        TextExtractor { doc, shared: Shared::default() }
    }

    /// The text of one page: lines end with `\n`, there is no page separator.
    /// A page without text (a scan with no text layer) gives an empty string.
    /// Errors: [`Error::PasswordRequired`] for a locked file, [`Error::Limit`]
    /// when the page asks for too much (more operators or characters than the
    /// limits, or the document's decoding budget is spent). Everything else that is
    /// wrong in the page (a content stream that cannot be decoded, a broken font or
    /// CMap, forms that contain themselves) is skipped and reported by
    /// [`TextExtractor::take_warnings`].
    pub fn page_text(&mut self, page: &Page) -> Result<String> {
        if self.doc.is_locked() {
            return Err(Error::PasswordRequired);
        }
        let content = page_content(self.doc, page, &mut self.shared.warnings)?;
        let scope = Rc::new(Scope::new(self.doc, page.resources()));
        let mut interp = Interp::new(self.doc, &mut self.shared);
        interp.run(&content, &scope, 0)?;
        let mut glyphs = std::mem::take(&mut interp.glyphs);
        let unmapped = std::mem::take(&mut interp.unmapped);
        drop(interp);
        for name in unmapped.keys() {
            self.shared.warnings.add(format!(
                "font {name}: some characters have no Unicode value (no /ToUnicode, and no known character collection) and are left out"
            ));
        }
        // Text outside the page (printer's marks, objects parked beside the page) is not on the page.
        if let Some([x0, y0, x1, y1]) = visible_box(page) {
            glyphs.retain(|g| {
                let margin = 0.5 * g.size.max(1.0);
                g.x >= x0 - margin && g.x <= x1 + margin && g.y >= y0 - margin && g.y <= y1 + margin
            });
        }
        Ok(layout::render(&glyphs))
    }

    /// What went wrong without stopping the extraction so far (each kind once, at most 50),
    /// and forget it.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.shared.warnings.list)
    }
}

/// The page's `/Contents` decoded and joined (7.8.2: several streams are one program, split at token borders).
pub(crate) fn page_content(doc: &Document, page: &Page, warnings: &mut font::Warnings) -> Result<Vec<u8>> {
    let mut streams: Vec<Stream> = Vec::new();
    match page.dict.get("Contents").map(|o| doc.resolve(o)) {
        Some(Ok(Object::Stream(s))) => streams.push(s),
        Some(Ok(Object::Array(items))) => {
            for item in &items {
                if let Ok(Object::Stream(s)) = doc.resolve(item) {
                    streams.push(s);
                }
            }
        }
        Some(Err(Error::Limit(m))) => return Err(Error::Limit(m)),
        Some(Err(e)) => warnings.add(format!("the page's /Contents could not be read: {e}")),
        _ => {}
    }
    let mut out = Vec::new();
    for s in &streams {
        match doc.decode_stream(s) {
            Ok(data) => {
                if out.len().saturating_add(data.len()) > MAX_PAGE_CONTENT {
                    return Err(Error::Limit("a page's content streams decode to more than 256 MiB".to_string()));
                }
                out.extend_from_slice(&data);
                out.push(b'\n');
            }
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => warnings.add(format!("a content stream is skipped: {e}")),
        }
    }
    Ok(out)
}

/// The part of the page that is shown: the crop box inside the media box (14.11.2), in default user
/// space. `None` when the page gives no usable box.
pub(crate) fn visible_box(page: &Page) -> Option<[f64; 4]> {
    let b = match (page.crop_box(), page.media_box()) {
        (Some(c), Some(m)) => {
            let i = [c[0].max(m[0]), c[1].max(m[1]), c[2].min(m[2]), c[3].min(m[3])];
            if i[0] < i[2] && i[1] < i[3] { i } else { c }
        }
        (Some(b), None) | (None, Some(b)) => b,
        (None, None) => return None,
    };
    (b[0] < b[2] && b[1] < b[3] && b.iter().all(|v| v.is_finite())).then_some(b)
}

/// May the text be taken out of this file? A file whose author did not allow
/// "copy or extract text and graphics" (permission bit 5) is refused when it
/// was opened with the user password (or none); the owner password lifts it.
/// `password` is the one the caller opened the file with.
pub fn check_extraction_allowed(doc: &Document, password: &str) -> Result<()> {
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    let Some(encryption) = doc.encryption() else { return Ok(()) };
    if encryption.permissions.copy || has_owner_rights(doc, &encryption, password) {
        return Ok(());
    }
    let tampered = if encryption.perms_valid {
        ""
    } else {
        " (its permission check value /Perms does not agree with its flags /P, which someone may have edited, so it is taken to allow nothing)"
    };
    Err(Error::Invalid(format!(
        "the file's author did not allow copying or extracting text{tampered}; the owner password is needed (give it with --password)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;
    use interp::{MAX_FORM_DEPTH, MAX_GLYPHS, MAX_OPERATORS};

    /// A one-page document: object 3 is the page with these resources, 4 its content,
    /// `objs` and `streams` are further objects.
    fn page_doc(resources: &str, content: &[u8], objs: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Document {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources {resources} /Contents 4 0 R >>"));
        b.stream_obj(4, "", content);
        let mut top = 4;
        for (n, body) in objs {
            b.obj(*n, body);
            top = top.max(*n);
        }
        for (n, dict, data) in streams {
            b.stream_obj(*n, dict, data);
            top = top.max(*n);
        }
        Document::from_bytes(b.finish_classic(top + 1, "/Root 1 0 R")).expect("the test file opens")
    }

    fn run(doc: &Document) -> (Result<String>, Vec<String>) {
        let mut ex = TextExtractor::new(doc);
        let pages = doc.pages().expect("pages");
        let text = ex.page_text(&pages[0]);
        (text, ex.take_warnings())
    }

    fn text(doc: &Document) -> String {
        run(doc).0.expect("text")
    }

    const HELV: &str = "<< /Font << /F1 5 0 R >> >>";
    const HELV_OBJ: (u32, &str) = (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");

    fn latin(content: &str) -> String {
        text(&page_doc(HELV, content.as_bytes(), &[HELV_OBJ], &[]))
    }

    #[test]
    fn lines_and_word_spaces() {
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (Hello) Tj 0 -14 Td (World) Tj ET"), "Hello\nWorld\n");
        // kerning moves back a little, a word gap forward by a third of the size
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td [(Hel) 20 (lo) -300 (there)] TJ ET"), "Hello there\n");
        // pieces drawn in separate Tj along one baseline, with a gap or without
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (ab) Tj 40 0 Td (cd) Tj ET"), "ab cd\n");
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (ab) Tj 12 0 Td (cd) Tj ET"), "abcd\n");
        // character spacing is part of the advance, it does not make spaces by itself
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td 3 Tc (a) Tj (b) Tj ET"), "ab\n");
        // the quote operators move to the next line first
        assert_eq!(latin("BT /F1 12 Tf 14 TL 72 700 Td (one) Tj (two) ' 1 2 (three) \" ET"), "one\ntwo\nthree\n");
    }

    #[test]
    fn simple_font_encodings() {
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding << /BaseEncoding /WinAnsiEncoding \
                    /Differences [65 /Euro /fi /uni4E2D /g99] >> >>";
        let doc = page_doc(HELV, b"BT /F1 12 Tf (ABCD\\223E\\200) Tj ET", &[(5, font)], &[]);
        assert_eq!(text(&doc), "€fi中“E€\n");
        // the Standard encoding is the default for Type 1 (code 174 is fi), MacRoman is its own table
        let std_font = "<< /Type /Font /Subtype /Type1 /BaseFont /Foo >>";
        assert_eq!(text(&page_doc(HELV, b"BT /F1 12 Tf (a\\256) Tj ET", &[(5, std_font)], &[])), "afi\n");
        let mac = "<< /Type /Font /Subtype /Type1 /BaseFont /Foo /Encoding /MacRomanEncoding >>";
        assert_eq!(text(&page_doc(HELV, b"BT /F1 12 Tf (\\216\\245) Tj ET", &[(5, mac)], &[])), "é•\n");
        // Symbol
        let sym = "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>";
        assert_eq!(text(&page_doc(HELV, b"BT /F1 12 Tf (ab) Tj ET", &[(5, sym)], &[])), "αβ\n");
    }

    #[test]
    fn tounicode_comes_first_and_ligatures_split() {
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Foo /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>";
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <41> <0058> <01> <FB01> endbfchar \
                     1 beginbfrange <61> <63> <0078> endbfrange";
        let doc = page_doc(HELV, b"BT /F1 12 Tf (ABabc\\001) Tj ET", &[(5, font)], &[(6, "", cmap)]);
        assert_eq!(text(&doc), "XBxyzfi\n");
    }

    #[test]
    fn composite_fonts() {
        // Identity-H, ToUnicode, widths from /W
        let type0 = "<< /Type /Font /Subtype /Type0 /BaseFont /Foo /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>";
        let cid = "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Foo /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                   /DW 1000 /W [1 [500] 2 3 250] >>";
        let tu = b"1 begincodespacerange <0000> <FFFF> endcodespacerange 3 beginbfchar <0001> <4E2D> <0002> <6587> <0003> <0041> endbfchar";
        let doc = page_doc(HELV, b"BT /F1 12 Tf <000100020003> Tj ET", &[(5, type0), (6, cid)], &[(7, "", tu)]);
        assert_eq!(text(&doc), "中文A\n");

        // no ToUnicode: the collection of the CIDSystemInfo (Adobe-GB1: CID 34 is "A")
        let type0 = "<< /Type /Font /Subtype /Type0 /BaseFont /Foo /Encoding /Identity-H /DescendantFonts [6 0 R] >>";
        let cid = "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /Foo /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> >>";
        let doc = page_doc(HELV, b"BT /F1 12 Tf <002200230024> Tj ET", &[(5, type0), (6, cid)], &[]);
        assert_eq!(text(&doc), "ABC\n");

        // a legacy predefined CMap: GBK code 0xB0A1 is U+554A
        let type0 = "<< /Type /Font /Subtype /Type0 /BaseFont /Foo /Encoding /GBK-EUC-H /DescendantFonts [6 0 R] >>";
        let doc = page_doc(HELV, b"BT /F1 12 Tf (\\260\\241a) Tj ET", &[(5, type0), (6, cid)], &[]);
        assert_eq!(text(&doc), "啊a\n");

        // an embedded CMap that builds on a predefined one
        let type0 = "<< /Type /Font /Subtype /Type0 /BaseFont /Foo /Encoding 8 0 R /DescendantFonts [6 0 R] >>";
        let cmap = b"/GBK-EUC-H usecmap 1 begincidchar <b0a2> 34 endcidchar";
        let doc = page_doc(HELV, b"BT /F1 12 Tf (\\260\\242\\260\\241) Tj ET", &[(5, type0), (6, cid)], &[(8, "/UseCMap /GBK-EUC-H", cmap)]);
        // code B0A2 is mapped to CID 34 ("A") by the embedded CMap, B0A1 stays as the predefined one has it
        assert_eq!(text(&doc), "A啊\n");
    }

    #[test]
    fn hidden_text_and_fake_bold() {
        assert_eq!(latin("BT /F1 12 Tf 3 Tr 72 700 Td (hidden) Tj ET"), "hidden\n");
        // the same text drawn again 0.3 units to the right, as a string and as single characters
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (Bold) Tj 0.3 0 Td (Bold) Tj ET"), "Bold\n");
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (B) Tj 0.2 0 Td (B) Tj ET"), "B\n");
        // two real letters side by side stay
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (oo) Tj ET"), "oo\n");
    }

    #[test]
    fn text_outside_the_page_is_not_on_the_page() {
        // printer's marks and objects parked beside the page are left out; text touching the edge stays
        assert_eq!(latin("BT /F1 12 Tf 72 700 Td (inside) Tj ET BT /F1 12 Tf 1 0 0 1 2000 400 Tm (far) Tj ET BT /F1 12 Tf 1 0 0 1 72 -50 Tm (below) Tj ET"), "inside\n");
        assert_eq!(latin("BT /F1 12 Tf 1 0 0 1 -3 400 Tm (edge) Tj ET"), "edge\n");
    }

    #[test]
    fn many_glyphs_on_one_spot_do_not_take_quadratic_time() {
        // 2000 copies of one letter at the same place (the glyph limit of the tests is 5000): all but one are repeats
        let content = format!("BT /F1 12 Tf 72 700 Td {} ET", "(a) Tj -6 0 Td 6 0 Td ".repeat(2000));
        let started = std::time::Instant::now();
        assert_eq!(latin(&content), "a\n");
        assert!(started.elapsed().as_secs() < 5);
    }

    #[test]
    fn turned_text_is_a_line_of_its_own() {
        let got = latin("BT /F1 12 Tf 0 1 -1 0 300 100 Tm (side) Tj ET BT /F1 12 Tf 1 0 0 1 72 700 Tm (top) Tj ET");
        assert_eq!(got, "side\ntop\n");
    }

    #[test]
    fn form_xobjects() {
        let res = "<< /XObject << /Fm1 6 0 R >> >>";
        let form = b"BT /F1 12 Tf (Inside) Tj ET";
        let form_dict = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [1 0 0 1 0 -20] /Resources << /Font << /F1 5 0 R >> >>";
        let doc = page_doc(res, b"1 0 0 1 100 700 cm /Fm1 Do 1 0 0 1 0 -30 cm /Fm1 Do", &[HELV_OBJ], &[(6, form_dict, form)]);
        assert_eq!(text(&doc), "Inside\nInside\n");

        // a form that shows itself: the repeat is skipped, with a warning
        let form = b"BT /F1 12 Tf (Once) Tj ET /Fm1 Do";
        let form_dict = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >>";
        let doc = page_doc(res, b"1 0 0 1 100 700 cm /Fm1 Do", &[HELV_OBJ], &[(6, form_dict, form)]);
        let (got, warnings) = run(&doc);
        assert_eq!(got.expect("text"), "Once\n");
        assert!(warnings.iter().any(|w| w.contains("contains itself")), "{warnings:?}");

        // two forms that show each other
        let a = b"BT /F1 12 Tf (A) Tj ET /B Do";
        let b = b"BT /F1 12 Tf (B) Tj ET /A Do";
        let dict_a = "/Subtype /Form /BBox [0 0 1 1] /Resources << /Font << /F1 5 0 R >> /XObject << /B 7 0 R >> >>";
        let dict_b = "/Subtype /Form /BBox [0 0 1 1] /Matrix [1 0 0 1 0 -20] /Resources << /Font << /F1 5 0 R >> /XObject << /A 6 0 R >> >>";
        let doc = page_doc("<< /XObject << /A 6 0 R >> >>", b"1 0 0 1 100 700 cm /A Do", &[HELV_OBJ], &[(6, dict_a, a), (7, dict_b, b)]);
        let (got, warnings) = run(&doc);
        assert_eq!(got.expect("text"), "A\nB\n");
        assert!(warnings.iter().any(|w| w.contains("contains itself")), "{warnings:?}");
    }

    #[test]
    fn forms_nested_too_deep_are_cut() {
        // form 6 shows form 7 shows form 8 ... each with its own text line
        let mut streams: Vec<(u32, String, Vec<u8>)> = Vec::new();
        for k in 1..=40u32 {
            let next = if k < 40 { "/X Do" } else { "" };
            streams.push((
                5 + k,
                format!("/Subtype /Form /BBox [0 0 1 1] /Matrix [1 0 0 1 0 -20] /Resources << /Font << /F1 5 0 R >> /XObject << /X {} 0 R >> >>", 6 + k),
                format!("BT /F1 12 Tf (n{k}) Tj ET {next}").into_bytes(),
            ));
        }
        let refs: Vec<(u32, &str, &[u8])> = streams.iter().map(|(n, d, c)| (*n, d.as_str(), c.as_slice())).collect();
        let doc = page_doc("<< /XObject << /X 6 0 R >> >>", b"1 0 0 1 100 770 cm /X Do", &[HELV_OBJ], &refs);
        let (got, warnings) = run(&doc);
        let got = got.expect("text");
        assert_eq!(got.lines().count(), MAX_FORM_DEPTH, "{got}");
        assert!(warnings.iter().any(|w| w.contains("nested more than")), "{warnings:?}");
    }

    #[test]
    fn damaged_content_streams() {
        // cut off inside a string, inside a dictionary, inside an array
        assert_eq!(latin("BT /F1 12 Tf (abc) Tj (def"), "abc\n");
        assert_eq!(latin("BT /F1 12 Tf (abc) Tj /Span <</ActualText (x"), "abc\n");
        assert_eq!(latin("BT /F1 12 Tf (abc) Tj [(d) 4"), "abc\n");
        // operands of the wrong type, too few, too many, operators that mean nothing
        let junk = "BT /F1 (x) Tf Td Tj 1 2 3 Tj [ ] TJ /F1 /F1 Tf 1 Tr (a) Tj << >> Tm 5 5 5 5 5 5 5 5 Tm ( ) ' \" [[[ ]]] ET >> ) } (ok) Tj \
                    endstream endobj 1e5 .. -- Q Q Q";
        assert!(run(&page_doc(HELV, junk.as_bytes(), &[HELV_OBJ], &[])).0.is_ok());
        // no font, no resources, text before any font
        assert_eq!(text(&page_doc("<< >>", b"BT (no font) Tj ET", &[], &[])), "");
        assert_eq!(text(&page_doc("<< /Font 99 0 R >>", b"BT /F1 12 Tf (x) Tj ET", &[], &[])), "");
        // numbers that overflow
        let big = "BT /F1 1e30 Tf 1e308 1e308 Td 99999999999999999999999999 0 0 99999999999999999999999999 1 1 Tm (big) Tj (big) Tj ET";
        assert!(run(&page_doc(HELV, big.as_bytes(), &[HELV_OBJ], &[])).0.is_ok());
        // binary data after ID
        assert_eq!(latin("BI /W 1 /H 1 /BPC 8 /CS /G ID \u{1}\u{2} EI BT /F1 12 Tf (after) Tj ET"), "after\n");
    }

    #[test]
    fn damaged_fonts_and_cmaps() {
        // no entries at all, a Type0 without descendants, widths that are not numbers, Type 3 with a nonsense matrix
        for font in [
            "<< >>",
            "<< /Type /Font /Subtype /Type0 >>",
            "<< /Type /Font /Subtype /Type0 /DescendantFonts [] /Encoding /NoSuchCMap-H >>",
            "<< /Type /Font /Subtype /Type1 /Widths [/a (b) null] /FirstChar -5 /Encoding 7 >>",
            "<< /Type /Font /Subtype /Type3 /FontMatrix [0 0 0] /Encoding << /Differences [-1 /a 999999 /b] >> >>",
            "<< /Type /Font /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [5 0 R] >>",
        ] {
            let doc = page_doc(HELV, b"BT /F1 12 Tf (abc\\000\\377) Tj <00010002> Tj ET", &[(5, font)], &[]);
            assert!(run(&doc).0.is_ok(), "{font}");
        }
        // broken CMaps: a ToUnicode, an embedded Encoding CMap, and a /UseCMap that loops
        let font = "<< /Type /Font /Subtype /Type0 /Encoding 8 0 R /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>";
        let cid = "<< /Type /Font /Subtype /CIDFontType0 /CIDSystemInfo << /Ordering (GB1) >> /W [1 2 3 4 [5] /x] /W2 [1 [1 2]] >>";
        let junk = b"1 begincodespacerange <00 <FF> 3 begincidrange <00> <ff> endcidrange 2 beginbfchar <41> endbfchar (((";
        let doc = page_doc(HELV, b"BT /F1 12 Tf <00410042> Tj ET", &[(5, font), (6, cid)], &[(7, "", junk), (8, "/UseCMap 8 0 R", junk)]);
        let (got, warnings) = run(&doc);
        assert!(got.is_ok());
        assert!(warnings.iter().any(|w| w.contains("damaged")), "{warnings:?}");
        // a ToUnicode that is not a stream
        let font = "<< /Type /Font /Subtype /Type1 /ToUnicode 9 0 R >>";
        assert!(run(&page_doc(HELV, b"BT /F1 12 Tf (a) Tj ET", &[(5, font), (9, "(text)")], &[])).0.is_ok());
    }

    #[test]
    fn limits_end_in_an_error_not_in_a_hang() {
        let many_ops = "q Q ".repeat(MAX_OPERATORS);
        let doc = page_doc(HELV, many_ops.as_bytes(), &[HELV_OBJ], &[]);
        assert!(matches!(run(&doc).0, Err(Error::Limit(_))));
        let long = format!("BT /F1 12 Tf ({}) Tj ET", "a".repeat(MAX_GLYPHS + 1));
        let doc = page_doc(HELV, long.as_bytes(), &[HELV_OBJ], &[]);
        assert!(matches!(run(&doc).0, Err(Error::Limit(_))));
        // forms that show each other many times over: the cycle is cut, the operator count bounds the rest
        let form1 = "/B Do ".repeat(200);
        let form2 = "/C Do ".repeat(200);
        let dict1 = "/Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /B 7 0 R >> >>";
        let dict2 = "/Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /C 6 0 R >> >>";
        let doc = page_doc("<< /XObject << /C 6 0 R >> >>", b"/C Do", &[HELV_OBJ], &[(6, dict1, form1.as_bytes()), (7, dict2, form2.as_bytes())]);
        let got = run(&doc).0;
        assert!(got.is_ok() || matches!(got, Err(Error::Limit(_))));
    }

    #[test]
    fn a_page_without_contents_is_empty() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
        let doc = Document::from_bytes(b.finish_classic(4, "/Root 1 0 R")).expect("opens");
        assert_eq!(text(&doc), "");
    }

    #[test]
    fn a_content_stream_that_cannot_be_decoded_is_skipped_with_a_warning() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents [4 0 R 6 0 R] >>");
        b.stream_obj(4, "/Filter /LZWDecode", b"nonsense");
        b.obj(5, HELV_OBJ.1);
        b.stream_obj(6, "", b"BT /F1 12 Tf (second) Tj ET");
        let doc = Document::from_bytes(b.finish_classic(7, "/Root 1 0 R")).expect("opens");
        let (got, warnings) = run(&doc);
        assert_eq!(got.expect("text"), "second\n");
        assert!(warnings.iter().any(|w| w.contains("skipped")), "{warnings:?}");
    }
}
