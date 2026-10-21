//! Redaction (item 8): the engine's own tests, on files made by hand. What was under a mark is looked for in the new file the way someone who wanted
//! it back would (the text, every stream decoded, every string of every object), and what was not under it is looked at to be as it was. The
//! other readers are checked by `tests/tools/redact_check.py`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use super::redact::{Area, FindSpec, Preset, RedactSpec, Redacted, redact_file};
use super::*;
use crate::object::{ObjRef, Object};
use crate::testutil::PdfBuilder;
use crate::text::{Shown, TextExtractor};

const NOW: &str = "D:20260101000000Z";

/// The widths of Helvetica, the thousandths of the standard 14 font (the engine guesses where a font has none, which a test of places cannot).
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278,
    500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 222, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500,
    278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

fn helvetica() -> String {
    let widths: Vec<String> = HELV.iter().map(u16::to_string).collect();
    format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 /LastChar 126 /Widths [{}] >>", widths.join(" "))
}

/// The objects 1 to 5 of a file of one page of 612 by 792 points: the catalog (with `catalog` more entries), the page tree, the page (`resources` and
/// `page_extra` more entries of it), its content and the font /F1 (`font`).
fn builder(catalog: &str, font: &str, content: &str, resources: &str, page_extra: &str) -> PdfBuilder {
    let mut b = PdfBuilder::new();
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"));
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> {resources} >> {page_extra} >>"));
    b.stream_obj(4, "", content.as_bytes());
    b.obj(5, font);
    b
}

/// A file of one page of 612 by 792 points: `content` is its content, `font` is /F1, `page_extra` more entries of the page, `extra` more objects.
fn one_page(content: &str, resources: &str, page_extra: &str, extra: &[(u32, &str)]) -> Vec<u8> {
    let mut b = builder("", &helvetica(), content, resources, page_extra);
    let mut top = 5;
    for (n, body) in extra {
        b.obj(*n, body);
        top = top.max(*n);
    }
    b.finish_classic(top + 1, "/Root 1 0 R")
}

fn spec(words: &[&str], areas: Vec<Area>) -> RedactSpec {
    RedactSpec { find: FindSpec { words: words.iter().map(ToString::to_string).collect(), presets: Vec::new() }, areas, color: [0, 0, 0], label: String::new(), use_marks: true, clear_metadata: false, remove_active: false }
}

fn redact(bytes: &[u8], spec: &RedactSpec) -> (Redacted, Document) {
    let doc = Document::from_bytes(bytes.to_vec()).unwrap();
    let done = redact_file(&doc, "", spec).unwrap();
    let out = Document::from_bytes(done.data.clone()).unwrap();
    (done, out)
}

/// The text of page `page`, and the boxes of its characters.
fn read(doc: &Document, page: usize) -> (String, Vec<[f32; 4]>) {
    let pages = doc.pages().unwrap();
    let chars = TextExtractor::new(doc).page_chars(&pages[page], &Work::new()).unwrap();
    (chars.text, chars.boxes)
}

/// Every stream of the file decoded, and every object printed: what a program that reads the file for what is in it sees.
fn everything(doc: &Document) -> Vec<u8> {
    let mut all = Vec::new();
    for n in 1..doc.next_free_number() {
        match doc.get(ObjRef::new(n, 0)) {
            Ok(Object::Stream(s)) => {
                crate::writer::write_object(&mut all, &Object::Dict(s.dict.clone())).unwrap();
                if let Ok(d) = doc.decode_stream(&s) {
                    all.extend(d);
                }
            }
            Ok(o) => crate::writer::write_object(&mut all, &o).unwrap(),
            Err(_) => {}
        }
        all.push(b'\n');
    }
    all
}

fn has(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn content_of(doc: &Document, page: usize) -> String {
    let p = &doc.pages().unwrap()[page];
    let mut out = Vec::new();
    match doc.resolve(p.dict.get("Contents").unwrap()).unwrap() {
        Object::Stream(s) => out = doc.decode_stream(&s).unwrap(),
        Object::Array(items) => {
            for i in items {
                if let Object::Stream(s) = doc.resolve(&i).unwrap() {
                    out.extend(doc.decode_stream(&s).unwrap());
                    out.push(b'\n');
                }
            }
        }
        _ => {}
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn text_under_a_mark_is_cut_out_and_what_follows_stays_where_it_was() {
    let content = "BT /F1 12 Tf 72 700 Td (Hello World, again) Tj 0 -20 Td [(Con) 30 (fid) -250 (ential) 15 (A)] TJ 0 -20 Td 14 TL (one) Tj (two secret) ' ET\n";
    let original = one_page(content, "", "", &[]);
    let before = Document::from_bytes(original.clone()).unwrap();
    let (text0, boxes0) = read(&before, 0);
    let (done, after) = redact(&original, &spec(&["World", "fid", "two secret"], vec![]));
    let (text1, boxes1) = read(&after, 0);
    assert_eq!(done.marks, 3, "{:?}", done.unfound);
    assert!(text0.contains("Hello World, again") && text0.contains("Confid") && text0.contains("two secret"), "{text0}");
    assert!(!text1.contains("World") && !text1.contains("fid") && !text1.contains("two"), "{text1}");
    assert!(text1.contains("Hello ") && text1.contains(", again") && text1.contains("Con") && text1.contains("ential") && text1.contains("one"), "{text1}");
    // Every character that is left is where it was: the same box for the comma, for the "e" of "ential" (after a number of the TJ that stays) and for
    // the "A" at the end of that line.
    let at = |text: &str, boxes: &[[f32; 4]], pat: &str| -> [f32; 4] {
        let i = text.find(pat).unwrap_or_else(|| panic!("{pat:?} is not in {text:?}"));
        boxes[text[..i].chars().count()]
    };
    for pat in [", again", "ential", "A\n"] {
        let (b0, b1) = (at(&text0, &boxes0, pat), at(&text1, &boxes1, pat));
        assert!((b0[0] - b1[0]).abs() < 0.02 && (b0[2] - b1[2]).abs() < 0.02 && (b0[1] - b1[1]).abs() < 0.02, "{pat:?} moved from {b0:?} to {b1:?} in {text1:?}");
    }
    // The cut text is not in the new file in any stream or string, and a box was drawn.
    let all = everything(&after);
    for gone in ["World", "fid", "two secret", "(Wo", "Wor"] {
        assert!(!has(&all, gone), "{gone} is still in the file");
    }
    let new = content_of(&after, 0);
    assert!(new.contains("re f"), "{new}");
    assert!(has(&all, "again") && has(&all, "one") && has(&all, "ential"));
}

#[test]
fn invisible_text_and_the_actual_text_of_a_sequence_are_taken_out_too() {
    let content = "BT /F1 12 Tf 72 700 Td (visible secret) Tj ET\n/Span <</ActualText (secret words)>> BDC BT 3 Tr /F1 12 Tf 72 600 Td (secret words) Tj ET EMC\n";
    let original = one_page(content, "", "", &[]);
    let (done, after) = redact(&original, &spec(&["secret"], vec![]));
    assert_eq!(done.marks, 2);
    let all = everything(&after);
    assert!(!has(&all, "secret"), "{}", String::from_utf8_lossy(&all));
    assert!(!has(&all, "ActualText"), "the entry went with the text");
    let (text, _) = read(&after, 0);
    assert!(text.contains("visible") && text.contains("words") && !text.contains("secret"), "{text}");
}

#[test]
fn a_form_two_pages_draw_is_copied_for_the_page_that_is_redacted() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>");
    let res = "<< /XObject << /Fm 8 0 R >> /Font << /F1 5 0 R >> >>";
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources {res} >>"));
    b.stream_obj(4, "", b"q 1 0 0 1 20 100 cm /Fm Do Q");
    b.obj(5, &helvetica());
    b.obj(6, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 7 0 R /Resources {res} >>"));
    b.stream_obj(7, "", b"q 1 0 0 1 20 100 cm /Fm Do Q");
    b.stream_obj(8, "/Type /XObject /Subtype /Form /BBox [0 0 200 100]", b"BT /F1 12 Tf 10 50 Td (FORMSECRET one) Tj 0 -20 Td (stays) Tj ET");
    let original = b.finish_classic(9, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [28.0, 132.0, 120.0, 156.0] }]));
    assert_eq!(done.pages.len(), 1);
    let (t0, _) = read(&after, 0);
    let (t1, _) = read(&after, 1);
    assert!(!t0.contains("FORMSECRET") && t0.contains("stays"), "{t0}");
    assert!(t1.contains("FORMSECRET one") && t1.contains("stays"), "the other page keeps its text: {t1}");
}

#[test]
fn a_picture_is_painted_where_the_mark_is_and_a_picture_drawn_twice_is_copied() {
    let samples: Vec<u8> = (0..16 * 16).map(|i| 40 + (i % 200) as u8).collect();
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents 4 0 R /Resources << /XObject << /Im 5 0 R >> >> >>");
    // The picture is drawn twice, 160 points wide at (20, 100) and at (200, 100); the mark is over the left one only.
    b.stream_obj(4, "", b"q 160 0 0 160 20 100 cm /Im Do Q q 160 0 0 160 200 100 cm /Im Do Q");
    b.stream_obj(5, "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceGray /BitsPerComponent 8", &samples);
    let original = b.finish_classic(6, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [60.0, 70.0, 100.0, 110.0] }]));
    assert_eq!(done.pages[0].1.images, 1);
    let page = &after.pages().unwrap()[0];
    let Object::Dict(res) = after.resolve(page.resources().unwrap()).unwrap() else { panic!() };
    let Some(Object::Dict(xo)) = res.get("XObject") else { panic!() };
    // The right-hand picture is the old one; the left-hand one is a new picture.
    let mut grays: Vec<Vec<u8>> = xo.iter().filter_map(|(_, v)| match after.resolve(v).ok()? {
        Object::Stream(s) => after.decode_stream(&s).ok(),
        _ => None,
    }).collect();
    assert_eq!(grays.len(), 2, "{xo:?}");
    grays.sort();
    let (painted, kept) = (&grays[0], &grays[1]);
    assert_eq!(kept, &samples);
    // x 60 to 100 is columns 4 to 9 (10 points a column from 20), y 70 to 110 from the bottom is rows from the top 10 - ... a block painted black.
    let black = painted.iter().filter(|&&v| v == 0).count();
    let changed = painted.iter().zip(&samples).filter(|(a, b)| a != b).count();
    assert!(black >= 12 && changed == black.min(changed) && changed <= 40, "{black} black, {changed} changed");
    assert!(painted.iter().zip(&samples).all(|(p, s)| p == s || *p == 0));
}

#[test]
fn an_inline_picture_is_made_a_picture_object_and_painted() {
    let mut raw = b"q 80 0 0 80 100 100 cm BI /W 8 /H 8 /CS /G /BPC 8 ID ".to_vec();
    raw.extend((0..64).map(|i| 100 + i as u8));
    raw.extend(b"\nEI Q\n");
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << >> >>");
    b.stream_obj(4, "", &raw);
    let original = b.finish_classic(5, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [100.0, 120.0, 140.0, 160.0] }]));
    assert_eq!(done.pages[0].1.images, 1);
    let content = content_of(&after, 0);
    assert!(!content.contains("BI") && content.contains(" Do"), "{content}");
    let page = &after.pages().unwrap()[0];
    let Object::Dict(res) = after.resolve(page.resources().unwrap()).unwrap() else { panic!() };
    let Some(Object::Dict(xo)) = res.get("XObject") else { panic!("{res:?}") };
    let Object::Stream(s) = after.resolve(xo.iter().next().unwrap().1).unwrap() else { panic!() };
    let pixels = after.decode_stream(&s).unwrap();
    assert_eq!(pixels.len(), 64);
    assert!(pixels.iter().filter(|&&v| v == 0).count() >= 8);
}

#[test]
fn a_shape_wholly_inside_a_mark_is_not_drawn_and_one_partly_inside_stays() {
    let content = "q 1 0 0 rg 100 100 20 20 re f Q q 0 0 1 RG 3 w 100 300 m 140 300 l S Q 0 1 0 rg 50 100 100 20 re f q 10 10 m 20 10 l 20 20 l W n 0.5 g 105 105 5 5 re f Q\n";
    let original = one_page(content, "", "", &[]);
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [90.0, 660.0, 150.0, 700.0] }]));
    let c = content_of(&after, 0);
    // The small square (100,100)-(120,120) is the page's y 672-692 from the top: inside the box; the wide green bar goes out of it: it stays.
    assert_eq!(done.pages[0].1.paths, 2, "{c}");
    assert!(!c.contains("100 100 20 20 re") && !c.contains("105 105 5 5 re"), "{c}");
    assert!(c.contains("50 100 100 20 re f") && c.contains("100 300 m 140 300 l S"), "{c}");
    // The clip is not a drawing: it stays.
    assert!(c.contains("W n"), "{c}");
}

#[test]
fn annotations_under_a_mark_go_with_their_popups_and_form_fields_with_their_values() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R] /XFA 20 0 R >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Contents 4 0 R /Annots [5 0 R 6 0 R 7 0 R 10 0 R 11 0 R 9 0 R] /Resources << >> >>");
    b.stream_obj(4, "", b"0 0 1 rg 0 0 5 5 re f");
    b.obj(5, "<< /Type /Annot /Subtype /Text /Rect [100 300 120 320] /Contents (secret note) /Popup 6 0 R >>");
    b.obj(6, "<< /Type /Annot /Subtype /Popup /Rect [300 300 380 380] /Parent 5 0 R >>");
    b.obj(7, "<< /Type /Annot /Subtype /Link /Rect [90 295 140 325] /A << /S /URI /URI (http://secret.example/) >> >>");
    b.obj(9, "<< /Type /Annot /Subtype /Text /Rect [300 50 320 70] /Contents (far away) >>");
    b.obj(10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (SECRETVALUE) /Rect [100 305 150 315] /Parent 12 0 R >>");
    b.obj(11, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (other) /V (stays) /Rect [300 100 350 115] >>");
    b.obj(12, "<< /T (group) /Kids [10 0 R] /V (SECRETVALUE) >>");
    b.obj(20, "<< /Type /Metadata /Length 5 >>\nstream\nSECRETVALUE\nendstream");
    let original = b.finish_classic(21, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [80.0, 70.0, 150.0, 110.0] }]));
    let all = everything(&after);
    assert!(!has(&all, "secret note") && !has(&all, "secret.example") && !has(&all, "SECRETVALUE"), "{}", String::from_utf8_lossy(&all));
    assert!(has(&all, "far away") && has(&all, "stays"));
    assert_eq!(done.pages[0].1.annots, 4, "{:?}", done.pages);
    let page = &after.pages().unwrap()[0];
    let Some(Object::Array(annots)) = page.dict.get("Annots").and_then(|a| after.resolve(a).ok()) else { panic!() };
    assert_eq!(annots.len(), 2);
}

#[test]
fn the_marks_in_the_file_are_applied_and_leave_no_trace() {
    let content = "BT /F1 12 Tf 72 700 Td (the number is 12345 here) Tj ET\n";
    let original = one_page(content, "", "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    let pages = doc.pages().unwrap();
    // A mark made the way the reader makes it: a box over "12345" (found: the reader gets the rectangle from the characters).
    let find = Edit::RedactFind { spec: Box::new(FindSpec { words: vec!["12345".to_string()], presets: Vec::new() }), author: "A".to_string() };
    let (objects, _) = apply_any(&doc, &pages, &find, NOW, &Work::new(), 1 << 24).unwrap();
    let changes = commit(&doc, objects);
    // The mark is an annotation: listed, with its quad.
    let annots = doc.pages().unwrap()[0].dict.get("Annots").and_then(|a| doc.resolve(a).ok());
    let Some(Object::Array(list)) = annots else { panic!("no annotations") };
    assert_eq!(list.len(), 1);
    let Object::Dict(mark) = doc.resolve(&list[0]).unwrap() else { panic!() };
    assert_eq!(mark.get_name("Subtype").unwrap(), &Name::from("Redact"));
    assert!(mark.contains_key("QuadPoints") && mark.contains_key("AP") && mark.contains_key("IC"));
    // Applied: the number is gone, so are the mark and its appearance.
    let done = redact_file(&doc, "", &RedactSpec { use_marks: true, color: [0, 0, 0], ..RedactSpec::default() }).unwrap();
    let out = Document::from_bytes(done.data).unwrap();
    let all = everything(&out);
    assert!(!has(&all, "12345") && !has(&all, "Redact"), "{}", String::from_utf8_lossy(&all));
    let (text, _) = read(&out, 0);
    assert!(text.contains("the number is") && text.contains("here") && !text.contains("12345"), "{text}");
    assert!(out.pages().unwrap()[0].dict.get("Annots").is_none());
    // The document is as it was after the redaction was worked out: the mark is still in it, and the mark can be undone like any edit.
    assert!(doc.pages().unwrap()[0].dict.get("Annots").is_some());
    replay(&doc, &changes, false);
    assert!(doc.pages().unwrap()[0].dict.get("Annots").is_none());
}

#[test]
fn kinds_of_number_are_found_by_their_shape_and_not_inside_longer_numbers() {
    let content = "BT /F1 12 Tf 72 700 Td (id 11010519491231002X, phone 13812345678, card 6222 0200 1234 5678 901.) Tj 0 -20 Td (not 138123456789 nor 12345678901234567890) Tj ET\n";
    let original = one_page(content, "", "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    let mut s = spec(&[], vec![]);
    s.find.presets = vec![Preset::IdCard, Preset::Phone, Preset::BankCard];
    let done = redact_file(&doc, "", &s).unwrap();
    assert_eq!(done.marks, 3, "{:?}", done.unfound);
    let out = Document::from_bytes(done.data).unwrap();
    let (text, _) = read(&out, 0);
    assert!(!text.contains("11010519") && !text.contains("13812345678 ") && !text.contains("6222") && !text.contains("phone 138"), "{text}");
    assert!(text.contains("138123456789") && text.contains("12345678901234567890"), "{text}");
}

#[test]
fn nothing_found_is_an_error_and_what_was_not_found_is_said() {
    let original = one_page("BT /F1 12 Tf 72 700 Td (abc) Tj ET\n", "", "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    assert!(redact_file(&doc, "", &spec(&["zzz"], vec![])).is_err());
    let done = redact_file(&doc, "", &spec(&["abc", "zzz"], vec![])).unwrap();
    assert_eq!(done.unfound, ["zzz"]);
    // A page that does not exist.
    assert!(redact_file(&doc, "", &spec(&[], vec![Area { page: 4, rect: [0.0, 0.0, 9.0, 9.0] }])).is_err());
}

#[test]
fn the_information_and_the_metadata_go_when_asked() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 9 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 20 100 Td (secret) Tj ET");
    b.obj(5, &helvetica());
    b.obj(8, "<< /Title (Secret Title) /Author (Someone) >>");
    b.stream_obj(9, "/Type /Metadata /Subtype /XML", b"<x:xmpmeta>Secret Author</x:xmpmeta>");
    let original = b.finish_classic(10, "/Root 1 0 R /Info 8 0 R");
    let keep = redact(&original, &spec(&["secret"], vec![])).1;
    assert!(has(&everything(&keep), "Secret Title") && has(&everything(&keep), "Secret Author"));
    let mut s = spec(&["secret"], vec![]);
    s.clear_metadata = true;
    let gone = redact(&original, &s).1;
    let all = everything(&gone);
    assert!(!has(&all, "Secret Title") && !has(&all, "Secret Author") && !has(&all, "Someone"), "{}", String::from_utf8_lossy(&all));
}

#[test]
fn a_page_whose_content_is_in_several_streams_is_written_as_one_and_a_statement_across_two_is_cut() {
    // The operands of a statement in one stream and its operator in the next; and a stream that the page lists twice.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents [4 0 R 6 0 R 7 0 R 7 0 R] /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 20 100 Td (secret)");
    b.obj(5, &helvetica());
    b.stream_obj(6, "", b" Tj ET");
    b.stream_obj(7, "", b"BT /F1 12 Tf 20 60 Td (secret again) Tj ET");
    let original = b.finish_classic(8, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&["secret"], vec![]));
    assert!(done.marks >= 2, "{:?}", done.unfound);
    let (text, _) = read(&after, 0);
    assert!(!text.contains("secret") && text.contains("again"), "{text}");
    assert!(!has(&everything(&after), "secret"));
    // (A page with its content in one stream: the new page has the stream it makes, between the two that keep what is open and close it.)
    let page = &after.pages().unwrap()[0];
    let Some(Object::Array(list)) = page.dict.get("Contents") else { panic!("{:?}", page.dict.get("Contents")) };
    assert_eq!(list.len(), 3);
}

#[test]
fn on_a_turned_page_the_box_covers_the_text_that_was_cut_and_nothing_else() {
    for rotate in [90, 180, 270] {
        let content = "BT /F1 18 Tf 72 700 Td (before SECRET after) Tj ET\n";
        let original = one_page(content, "", &format!("/Rotate {rotate}"), &[]);
        let before = Document::from_bytes(original.clone()).unwrap();
        let (text0, boxes0) = read(&before, 0);
        let (done, after) = redact(&original, &spec(&["SECRET"], vec![]));
        assert_eq!(done.marks, 1);
        let (text1, boxes1) = read(&after, 0);
        assert!(!text1.contains("SECRET") && text1.contains("before") && text1.contains("after"), "{text1}");
        // The box, from the content (the page as shown, y up, put into user space by the matrix before it), as the page is shown: it is the box
        // of the characters that went.
        let from = text0.find("SECRET").unwrap();
        let word: Vec<[f32; 4]> = boxes0[text0[..from].chars().count()..text0[..from].chars().count() + 6].to_vec();
        let want = [word.iter().map(|b| b[0]).fold(f32::MAX, f32::min), word.iter().map(|b| b[1]).fold(f32::MAX, f32::min), word.iter().map(|b| b[2]).fold(f32::MIN, f32::max), word.iter().map(|b| b[3]).fold(f32::MIN, f32::max)];
        let content = content_of(&after, 0);
        let numbers = |line: &str| -> Vec<f64> { line.split_whitespace().filter_map(|t| t.parse().ok()).collect() };
        let m = numbers(content.lines().find(|l| l.ends_with(" cm")).unwrap());
        let r = numbers(content.lines().find(|l| l.ends_with(" re f")).unwrap());
        let user = |x: f64, y: f64| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        let pages = after.pages().unwrap();
        let shown = Shown::of(&pages[0]);
        let (a, b) = (shown.map(user(r[0], r[1]).0, user(r[0], r[1]).1), shown.map(user(r[0] + r[2], r[1] + r[3]).0, user(r[0] + r[2], r[1] + r[3]).1));
        let got = [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)];
        for k in 0..4 {
            assert!((f64::from(want[k]) - got[k]).abs() < 0.6, "rotate {rotate}: the box is {got:?}, the characters were at {want:?}");
        }
        // What is left has not moved.
        let i = text1.find("after").unwrap();
        let j = text0.find("after").unwrap();
        let (b1, b0) = (boxes1[text1[..i].chars().count()], boxes0[text0[..j].chars().count()]);
        assert!((0..4).all(|k| (b1[k] - b0[k]).abs() < 0.02), "rotate {rotate}: {b1:?} against {b0:?}");
    }
}

/// The pictures of page 0 of `doc` that are not the page's own: (the picture's dictionary, its samples decoded).
fn pictures(doc: &Document) -> Vec<(crate::object::Dict, Vec<u8>)> {
    let page = &doc.pages().unwrap()[0];
    let Object::Dict(res) = doc.resolve(page.resources().unwrap()).unwrap() else { panic!() };
    let Some(Object::Dict(xo)) = res.get("XObject") else { return Vec::new() };
    xo.iter()
        .filter_map(|(_, v)| match doc.resolve(v).ok()? {
            Object::Stream(s) => Some((s.dict.clone(), doc.decode_stream(&s).ok()?)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_picture_with_a_soft_mask_is_painted_opaque_in_the_colour_and_keeps_its_mask_elsewhere() {
    // 16 by 16 colour pixels, each a different colour; the soft mask is 8 by 8 (half the size) and grows from 10 to 200.
    let colours: Vec<u8> = (0..16 * 16 * 3).map(|i| (i * 7 % 251) as u8 | 1).collect();
    let alpha: Vec<u8> = (0..8 * 8).map(|i| 10 + (i * 3) as u8).collect();
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im 5 0 R >> >> >>");
    b.stream_obj(4, "", b"q 160 0 0 160 20 20 cm /Im Do Q");
    b.stream_obj(5, "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R", &colours);
    b.stream_obj(6, "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8", &alpha);
    let original = b.finish_classic(7, "/Root 1 0 R");
    // The mark: x 20 to 60 and y 120 to 160 from the top of a page 200 high... picture rows from the top 0 to 4, columns 0 to 4 (10 points a pixel).
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [20.0, 20.0, 60.0, 60.0] }]));
    assert_eq!(done.pages[0].1.images, 1);
    let pics = pictures(&after);
    let image = pics.iter().find(|(d, _)| d.contains_key("SMask")).expect("the new picture has a soft mask");
    assert_eq!(image.1.len(), 16 * 16 * 3);
    let mask = pictures_mask(&after, &image.0);
    assert_eq!(mask.len(), 16 * 16);
    for y in 0..16usize {
        for x in 0..16usize {
            let under = x < 4 && y < 4;
            let px = &image.1[(y * 16 + x) * 3..(y * 16 + x) * 3 + 3];
            if under {
                assert_eq!((px, mask[y * 16 + x]), (&[0u8, 0, 0][..], 255), "{x},{y}");
            } else {
                // The mask was 8 by 8 and is 16 by 16 now: each of its pixels is four of the new.
                let a = alpha[(y / 2) * 8 + x / 2];
                assert_eq!(mask[y * 16 + x], a, "{x},{y}");
                // The colour is as it was, as near as the opacity lets it be (the drawing keeps colour times opacity, so a pixel that is nearly
                // transparent has the colour only to 255 / opacity).
                let tolerance = 255 / i32::from(a.max(1)) + 1;
                for (got, was) in px.iter().zip(&colours[(y * 16 + x) * 3..(y * 16 + x) * 3 + 3]) {
                    assert!((i32::from(*got) - i32::from(*was)).abs() <= tolerance, "{x},{y}: {px:?}");
                }
            }
        }
    }
}

fn pictures_mask(doc: &Document, image: &crate::object::Dict) -> Vec<u8> {
    let Some(Object::Stream(s)) = image.get("SMask").and_then(|m| doc.resolve(m).ok()) else { panic!("no soft mask") };
    doc.decode_stream(&s).unwrap()
}

#[test]
fn a_stencil_mask_stays_a_stencil_mask_with_the_pixels_under_the_mark_not_painted() {
    // 16 by 16 bits, all painted (0) but every fourth column.
    let mut bits = Vec::new();
    for _ in 0..16 {
        bits.extend_from_slice(&[0b1110_1110u8, 0b1110_1110]);
    }
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im 5 0 R >> >> >>");
    b.stream_obj(4, "", b"q 1 0 0 rg 160 0 0 160 20 20 cm /Im Do Q");
    b.stream_obj(5, "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ImageMask true /BitsPerComponent 1", &bits);
    let original = b.finish_classic(6, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&[], vec![Area { page: 0, rect: [60.0, 60.0, 100.0, 100.0] }]));
    assert_eq!(done.pages[0].1.images, 1);
    let pics = pictures(&after);
    assert_eq!(pics.len(), 1);
    let (dict, data) = &pics[0];
    assert!(matches!(dict.get("ImageMask"), Some(Object::Bool(true))));
    assert_eq!(data.len(), 32);
    // Pixels at columns 4 to 7 and rows 4 to 7 (10 points each, from x = 20 and from the top y = 40): not painted now (1); the rest as it was.
    for y in 0..16usize {
        for x in 0..16usize {
            let bit = |d: &[u8]| d[y * 2 + x / 8] >> (7 - x % 8) & 1;
            if (4..8).contains(&x) && (4..8).contains(&y) {
                assert_eq!(bit(data), 1, "{x},{y}");
            } else {
                assert_eq!(bit(data), bit(&bits), "{x},{y}");
            }
        }
    }
}

// --- what the review of 2026-10-21 found ----------------------------------------------------------------------------------------------------------

#[test]
fn a_font_without_widths_loses_every_line_a_mark_crosses_not_a_guess_of_where_the_letters_are() {
    let advance = |s: &str| s.bytes().map(|b| f64::from(HELV[usize::from(b) - 32])).sum::<f64>() * 12.0 / 1000.0;
    let line = "iiiiiiiiiiiiiiiiiiii SECRET NEEDLE ID 110105194912310021 llllllllll";
    let x0 = 72.0 + advance("iiiiiiiiiiiiiiiiiiii SECRET NEEDLE ID ");
    let x1 = x0 + advance("110105194912310021");
    let content = format!("BT /F1 12 Tf 72 700 Td ({line}) Tj 0 -100 Td (another line stays) Tj ET\n");
    let original = builder("", "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>", &content, "", "").finish_classic(6, "/Root 1 0 R");
    // The mark is over the digits where they really are (the baseline is 92 points from the top; the letters are 8 points high).
    let area = Area { page: 0, rect: [x0 as f32 + 0.5, 84.0, x1 as f32 - 0.5, 91.0] };
    let (done, after) = redact(&original, &spec(&[], vec![area]));
    let all = everything(&after);
    for gone in ["110105", "10021", "NEEDLE", "SECRET"] {
        assert!(!has(&all, gone), "{gone} is left: {}", String::from_utf8_lossy(&all));
    }
    assert!(has(&all, "another line stays"));
    assert!(done.warnings.iter().any(|w| w.contains("no widths") && w.contains("whole")), "{:?}", done.warnings);
}

#[test]
fn text_in_a_font_the_page_does_not_have_stops_the_redaction_and_a_form_without_fonts_uses_the_pages() {
    let original = one_page("BT /F9 12 Tf 72 700 Td (secret) Tj ET\nBT /F1 12 Tf 72 600 Td (visible) Tj ET\n", "", "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    let e = redact_file(&doc, "", &spec(&[], vec![Area { page: 0, rect: [60.0, 60.0, 200.0, 110.0] }])).unwrap_err();
    assert!(e.to_string().contains("not in the resources"), "{e}");
    // No font chosen at all is the same.
    let original = one_page("BT 72 700 Td (secret) Tj ET\n", "", "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    assert!(redact_file(&doc, "", &spec(&[], vec![Area { page: 0, rect: [60.0, 60.0, 200.0, 110.0] }])).is_err());
    // A form that has resources but no fonts takes the page's fonts, so its text is read (and found, and cut).
    let mut b = builder("", &helvetica(), "q 1 0 0 1 72 600 cm /Fm Do Q\n", "/XObject << /Fm 8 0 R >>", "");
    b.stream_obj(8, "/Type /XObject /Subtype /Form /BBox [0 0 300 40] /Resources << /ProcSet [/PDF] >>", b"BT /F1 12 Tf 0 10 Td (FORMSECRET words) Tj ET");
    let (done, after) = redact(&b.finish_classic(9, "/Root 1 0 R"), &spec(&["FORMSECRET"], vec![]));
    assert_eq!(done.marks, 1);
    let (text, _) = read(&after, 0);
    assert!(!text.contains("FORMSECRET") && text.contains("words"), "{text}");
}

#[test]
fn the_words_that_marked_content_carries_go_inline_through_the_resources_and_from_the_structure_tree() {
    let content = "/Span <</Alt (secret one) /E (secret two) /ActualText (secret three)>> BDC BT /F1 12 Tf 72 700 Td (secret text) Tj ET EMC\n\
                   /Span /P1 BDC BT /F1 12 Tf 72 600 Td (other words) Tj ET EMC\n\
                   /Figure /P2 BDC BT /F1 12 Tf 72 500 Td (more words) Tj ET EMC\n";
    let mut b = builder("/StructTreeRoot 10 0 R", &helvetica(), content, "/Properties << /P1 << /ActualText (secret four) /MCID 1 >> /P2 9 0 R >>", "");
    b.obj(9, "<< /Alt (secret five) /MCID 2 >>");
    b.obj(10, "<< /Type /StructTreeRoot /K 11 0 R >>");
    b.obj(11, "<< /Type /StructElem /S /Figure /P 10 0 R /Pg 3 0 R /K 0 /Alt (secret six) /ActualText (secret seven) >>");
    let (done, after) = redact(&b.finish_classic(12, "/Root 1 0 R"), &spec(&["secret text"], vec![]));
    assert_eq!(done.marks, 1);
    let all = everything(&after);
    assert!(!has(&all, "secret"), "{}", String::from_utf8_lossy(&all));
    assert!(has(&all, "other words") && has(&all, "more words") && has(&all, "MCID"));
}

#[test]
fn what_an_annotation_says_is_found_and_the_annotation_goes() {
    let mut b = builder("/AcroForm << /Fields [8 0 R] >>", &helvetica(), "BT /F1 12 Tf 72 700 Td (nothing here) Tj ET\n", "", "/Annots [8 0 R 9 0 R 10 0 R 11 0 R]");
    b.stream_obj(7, "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >>", b"BT /F1 12 Tf 2 5 Td (NEEDLEWORD) Tj ET");
    b.obj(8, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (f1) /V (NEEDLEWORD) /Rect [50 100 150 120] /AP << /N 7 0 R >> /F 4 >>");
    b.obj(9, "<< /Type /Annot /Subtype /FreeText /Rect [50 50 150 70] /Contents (a NEEDLEWORD in a note) /DA (/F1 12 Tf 0 g) /F 4 >>");
    b.obj(10, "<< /Type /Annot /Subtype /Highlight /Rect [50 20 150 40] /QuadPoints [50 40 150 40 50 20 150 20] /Contents (needleword) /F 4 >>");
    b.obj(11, "<< /Type /Annot /Subtype /Text /Rect [300 300 320 320] /Contents (far away) /F 4 >>");
    let original = b.finish_classic(12, "/Root 1 0 R");
    let (done, after) = redact(&original, &spec(&["needleword"], vec![]));
    assert_eq!(done.marks, 3, "{:?}", done.unfound);
    assert_eq!(done.pages[0].1.annots, 3);
    let all = everything(&after);
    assert!(!has(&all, "NEEDLEWORD") && !has(&all, "needleword") && has(&all, "far away"), "{}", String::from_utf8_lossy(&all));
    // A kind of number is found in what an annotation says too.
    let mut b = builder("", &helvetica(), "BT /F1 12 Tf 72 700 Td (nothing here) Tj ET\n", "", "/Annots [8 0 R]");
    b.obj(8, "<< /Type /Annot /Subtype /Text /Rect [50 100 70 120] /Contents (call 13812345678 now) /F 4 >>");
    let mut s = spec(&[], vec![]);
    s.find.presets = vec![Preset::Phone];
    let (done, after) = redact(&b.finish_classic(9, "/Root 1 0 R"), &s);
    assert_eq!((done.marks, done.pages[0].1.annots), (1, 1));
    assert!(!has(&everything(&after), "13812345678"));
}

#[test]
fn a_reply_to_a_note_that_goes_goes_too_and_the_page_loses_its_own_metadata() {
    let mut b = builder("", &helvetica(), "BT /F1 12 Tf 72 700 Td (nothing) Tj ET\n", "", "/Annots [8 0 R 9 0 R] /Metadata 10 0 R");
    b.obj(8, "<< /Type /Annot /Subtype /Text /Rect [100 300 120 320] /Contents (a note) >>");
    b.obj(9, "<< /Type /Annot /Subtype /Text /Rect [400 100 420 120] /Contents (reply to it) /IRT 8 0 R /RT /R >>");
    b.stream_obj(10, "/Type /Metadata /Subtype /XML", b"<x>PAGEMETA</x>");
    let (done, after) = redact(&b.finish_classic(11, "/Root 1 0 R"), &spec(&[], vec![Area { page: 0, rect: [90.0, 460.0, 130.0, 500.0] }]));
    assert_eq!(done.pages[0].1.annots, 2);
    let all = everything(&after);
    assert!(!has(&all, "reply to it") && !has(&all, "a note") && !has(&all, "PAGEMETA"), "{}", String::from_utf8_lossy(&all));
}

#[test]
fn what_is_left_in_the_file_is_counted_said_and_taken_out_when_asked() {
    let content = "BT /F1 12 Tf 72 700 Td (the secret name is here) Tj ET\n";
    let mut b = builder("/Outlines 20 0 R /Names << /EmbeddedFiles 22 0 R /JavaScript 24 0 R >>", &helvetica(), content, "", "");
    b.obj(20, "<< /Type /Outlines /First 21 0 R /Last 21 0 R /Count 1 >>");
    b.obj(21, "<< /Title (about the Secret Name) /Parent 20 0 R /Dest [3 0 R /Fit] >>");
    b.obj(22, "<< /Names [(a.txt) 23 0 R] >>");
    b.obj(23, "<< /Type /Filespec /F (a.txt) /EF << /F 25 0 R >> >>");
    b.stream_obj(25, "/Type /EmbeddedFile", b"the file says secret name");
    b.obj(24, "<< /Names [(init) 26 0 R] >>");
    b.obj(26, "<< /S /JavaScript /JS (app.alert\\('secret name'\\)) >>");
    let original = b.finish_classic(27, "/Root 1 0 R");
    let doc = Document::from_bytes(original).unwrap();
    // Not asked to take them out: they are counted, and so is where the text still is.
    let kept = redact_file(&doc, "", &spec(&["secret name"], vec![])).unwrap();
    let said = kept.warnings.join("\n");
    assert!(said.contains("1 attachment(s) and 1 script(s)") && said.contains("ask for them to be taken out"), "{said}");
    assert!(said.contains("bookmarks") && said.contains("names (of destinations, attachments, scripts)"), "{said}");
    let all = everything(&Document::from_bytes(kept.data).unwrap());
    assert!(has(&all, "the file says") && has(&all, "app.alert"));
    // Asked: they are gone, and so is their place in the list of what is left.
    let mut s = spec(&["secret name"], vec![]);
    s.remove_active = true;
    let gone = redact_file(&doc, "", &s).unwrap();
    let said = gone.warnings.join("\n");
    assert!(said.contains("Took out 1 attachment(s) and 1 script(s)") && said.contains("bookmarks") && !said.contains("names (of destinations"), "{said}");
    let all = everything(&Document::from_bytes(gone.data).unwrap());
    assert!(!has(&all, "the file says") && !has(&all, "app.alert") && !has(&all, "EmbeddedFile"), "{}", String::from_utf8_lossy(&all));
}

#[test]
fn a_pattern_that_nothing_is_painted_with_any_more_goes_with_the_text_in_it() {
    let pattern = "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 150 30] /XStep 150 /YStep 30 /Resources << /Font << /F1 5 0 R >> >>";
    let make = |content: &str| {
        let mut b = builder("", &helvetica(), content, "/Pattern << /P1 9 0 R >>", "");
        b.stream_obj(9, pattern, b"BT /F1 12 Tf 5 10 Td (PATTERNSECRET) Tj ET");
        b.finish_classic(10, "/Root 1 0 R")
    };
    // The only rectangle that the pattern paints is under the mark: the pattern goes. (The rectangle is 100 to 140 from the bottom: 652 to 692 from the top.)
    let area = Area { page: 0, rect: [60.0, 640.0, 300.0, 700.0] };
    let (_, after) = redact(&make("/Pattern cs /P1 scn 72 100 200 40 re f\n"), &spec(&[], vec![area.clone()]));
    assert!(!has(&everything(&after), "PATTERNSECRET"));
    // The pattern also paints a rectangle that stays: it stays.
    let (_, after) = redact(&make("/Pattern cs /P1 scn 72 100 200 40 re f 72 300 100 40 re f\n"), &spec(&[], vec![area]));
    assert!(has(&everything(&after), "PATTERNSECRET"));
}

#[test]
fn a_font_that_lost_text_is_said_to_still_list_it_in_its_table() {
    let table = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange \
                 3 beginbfchar <0001> <0053> <0002> <0045> <0003> <0043> endbfchar endcmap end end";
    let mut b = builder("", "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>", "BT /F1 12 Tf 72 700 Td <000100020003> Tj ET\n", "", "");
    b.obj(6, "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 600 >>");
    b.stream_obj(7, "", table.as_bytes());
    let (done, _) = redact(&b.finish_classic(8, "/Root 1 0 R"), &spec(&["SEC"], vec![]));
    assert_eq!(done.marks, 1);
    assert!(done.warnings.iter().any(|w| w.contains("/ToUnicode")), "{:?}", done.warnings);
    // A font without such a table: nothing to say.
    let (done, _) = redact(&one_page("BT /F1 12 Tf 72 700 Td (secret) Tj ET\n", "", "", &[]), &spec(&["secret"], vec![]));
    assert!(!done.warnings.iter().any(|w| w.contains("/ToUnicode")), "{:?}", done.warnings);
}

#[test]
fn a_page_that_cannot_be_read_is_said_by_a_find() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 20 100 Td (find me) Tj ET");
    b.obj(5, &helvetica());
    b.obj(6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 7 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(7, "/Filter /NoSuchFilter", b"BT /F1 12 Tf 20 100 Td (find me too) Tj ET");
    let doc = Document::from_bytes(b.finish_classic(8, "/Root 1 0 R")).unwrap();
    let pages = doc.pages().unwrap();
    let find = Edit::RedactFind { spec: Box::new(FindSpec { words: vec!["find me".to_string()], presets: Vec::new() }), author: "A".to_string() };
    let (_, notes) = apply_any(&doc, &pages, &find, NOW, &Work::new(), 1 << 24).unwrap();
    assert_eq!(notes.last().map(String::as_str), Some("1"), "the number of places is last: {notes:?}");
    assert!(notes.iter().any(|n| n.contains("Pages 2 ")), "{notes:?}");
}
