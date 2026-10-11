//! Changing text that is already on a page (5a): the engine's own tests, on files made by hand. The bytes of the new content are
//! compared exactly (only the statements of the operators that change are written again), text is read back with the text
//! extractor, and the limits (hits, work, memory) are met with files made to meet them. Tests that need a system font say so and
//! stop quietly where the machine has none.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use super::retext::MAX_HITS_PER_PAGE;
use super::*;
use crate::object::Object;
use crate::testutil::PdfBuilder;
use crate::text::TextExtractor;

const NOW: &str = "D:20260101000000Z";
const HELV: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

fn have_latin_font() -> bool {
    crate::render::sysfont::load(crate::render::sysfont::request_for(b"Helvetica", 32, 0.0, 0.0, None)).is_some()
}

fn have_chinese_font() -> bool {
    let chars: std::collections::BTreeSet<char> = "你好".chars().collect();
    fontsub::choose_like(&chars, crate::render::sysfont::request_for(b"Helvetica", 32, 0.0, 0.0, None)).is_ok()
}

/// A page of 612 by 792 points whose content is `content`, with the font `font` as /F1 (and `extra` as more objects, `resources`
/// as more of the page's resources).
fn one_page(content: &str, font: &str, resources: &str, extra: &[(u32, &str)]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> {resources} >> >>"));
    b.stream_obj(4, "", content.as_bytes());
    b.obj(5, font);
    let mut top = 5;
    for (n, body) in extra {
        b.obj(*n, body);
        top = top.max(*n);
    }
    b.finish_classic(top + 1, "/Root 1 0 R")
}

/// The text of every page, and the content of page `page` as written (its streams decoded and joined).
fn read(bytes: &[u8], page: usize) -> (String, String) {
    let doc = Document::from_bytes(bytes.to_vec()).unwrap();
    let pages = doc.pages().unwrap();
    let mut text = TextExtractor::new(&doc);
    let all: String = pages.iter().map(|p| text.page_text(p).unwrap()).collect();
    let mut content = Vec::new();
    match doc.resolve(pages[page].dict.get("Contents").unwrap()).unwrap() {
        Object::Stream(s) => content = doc.decode_stream(&s).unwrap(),
        Object::Array(items) => {
            for i in items {
                if let Object::Stream(s) = doc.resolve(&i).unwrap() {
                    content.extend(doc.decode_stream(&s).unwrap());
                }
            }
        }
        _ => {}
    }
    (all, String::from_utf8_lossy(&content).into_owned())
}

#[test]
fn a_word_is_taken_out_of_the_content_and_the_new_one_put_where_it_was() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    let content = "BT /F1 12 Tf 72 700 Td (Hello World) Tj 0 -14 Td [(Hel) 20 (lo) -300 (World)] TJ 0 -14 Td 14 TL (one) Tj (two) ' ET\n0 0 1 rg 10 10 20 20 re f\n";
    let original = one_page(content, HELV, "", &[]);
    let doc = Document::from_bytes(original.clone()).unwrap();
    let done = replace_text(&doc, "", "World", "Mars", None).unwrap();
    assert_eq!((done.count, done.pages), (2, 1), "{:?}", done.skipped);
    assert!(done.data.starts_with(&original), "the original bytes are a prefix");
    let (text, new) = read(&done.data, 0);
    assert_eq!(text, "Hello Mars\nHello Mars\none\ntwo\n");
    // Only the two statements change; the old letters are not in the content at all (nothing is covered over).
    assert_eq!(
        new,
        "BT /F1 12 Tf 72 700 Td (Hello Mars) Tj 0 -14 Td [(Hel) 20 (lo) -300 (Mars)] TJ 0 -14 Td 14 TL (one) Tj (two) ' ET\n0 0 1 rg 10 10 20 20 re f\n"
    );
    assert!(!new.contains("World"));
    assert!(done.warnings.is_empty(), "the new word is no wider than the old: {:?}", done.warnings);
    // A text that spans the spacing number of a TJ takes it with it; the line operators come out as T* and a Tj.
    let both = replace_text(&doc, "", "lo World", "p Mars", Some(&[0])).unwrap();
    let (text, new) = read(&both.data, 0);
    assert!(text.contains("Help Mars\nHelp Mars\n"), "{text}");
    assert!(new.contains("[(Hel) 20 (p Mars)] TJ"), "{new}");
    let quote = replace_text(&doc, "", "two", "2", None).unwrap();
    let (text, new) = read(&quote.data, 0);
    assert!(text.ends_with("one\n2\n"), "{text}");
    assert!(new.contains("14 TL (one) Tj T*\n(2) Tj ET"), "{new}");
    // Nothing found: nothing is written.
    let none = replace_text(&doc, "", "absent", "x", None).unwrap();
    assert_eq!((none.count, none.data.len()), (0, 0));
    assert!(replace_text(&doc, "", "a", "a", None).is_err() && replace_text(&doc, "", "", "a", None).is_err());
    assert!(replace_text(&doc, "", "a", "b\nc", None).is_err());
    assert!(replace_text(&doc, "", "a", "b", Some(&[3])).is_err());
}

#[test]
fn text_the_font_has_no_glyph_for_goes_in_a_substitute_font_and_back() {
    if !have_chinese_font() {
        println!("SKIPPED: no Chinese font on this machine");
        return;
    }
    let original = one_page("BT /F1 12 Tf 72 700 Td 0.5 Tc (Hello) Tj ET", HELV, "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    let done = replace_text(&doc, "", "Hello", "Hi 你好", None).unwrap();
    assert_eq!(done.count, 1);
    let (text, new) = read(&done.data, 0);
    assert_eq!(text, "Hi 你好\n");
    // The page's own font for what it has, the substitute (named after its object) switched to for the rest and switched back.
    assert!(new.starts_with("BT /F1 12 Tf 72 700 Td 0.5 Tc (Hi ) Tj\n/Qf"), "{new}");
    assert!(new.contains(" 12 Tf\n<") && new.trim_end().ends_with("/F1 12 Tf ET") || new.contains("Tj\n/F1 12 Tf"), "{new}");
    let doc = Document::from_bytes(done.data).unwrap();
    let page = &doc.pages().unwrap()[0];
    let Object::Dict(res) = doc.resolve(page.resources().unwrap()).unwrap() else { panic!("no resources") };
    let Some(Object::Dict(fonts)) = res.get("Font") else { panic!("no fonts") };
    assert!(fonts.iter().any(|(k, _)| k.0.starts_with(b"Qf")) && fonts.contains_key("F1"));
}

#[test]
fn a_form_that_two_pages_share_is_copied_for_the_page_that_is_changed_only() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    let form = "BT /F1 12 Tf 10 10 Td (Shared text) Tj ET";
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Fm1 20 0 R >> >> >>");
    b.stream_obj(4, "", b"q 1 0 0 1 100 500 cm /Fm1 Do Q");
    b.obj(6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /XObject << /Fm1 20 0 R >> >> >>");
    b.stream_obj(7, "", b"/Fm1 Do");
    b.obj(5, HELV);
    b.stream_obj(20, "/Type /XObject /Subtype /Form /BBox [0 0 300 100] /Resources << /Font << /F1 5 0 R >> >>", form.as_bytes());
    let original = b.finish_classic(21, "/Root 1 0 R");
    let doc = Document::from_bytes(original.clone()).unwrap();
    let done = replace_text(&doc, "", "Shared", "Own", Some(&[0])).unwrap();
    assert_eq!(done.count, 1);
    let (text, _) = read(&done.data, 0);
    assert_eq!(text, "Own text\nShared text\n", "the other page keeps its text");
    let changed = Document::from_bytes(done.data.clone()).unwrap();
    let fm1 = |doc: &Document, page: usize| {
        let p = &doc.pages().unwrap()[page];
        let Object::Dict(res) = doc.resolve(p.resources().unwrap()).unwrap() else { panic!() };
        let Some(Object::Dict(x)) = res.get("XObject") else { panic!() };
        x.get("Fm1").and_then(Object::as_obj_ref).unwrap().num
    };
    assert_ne!(fm1(&changed, 0), 20, "the page draws a copy");
    assert_eq!(fm1(&changed, 1), 20, "the other page draws the form it did");
    let Object::Stream(old) = changed.get(ObjRef::new(20, 0)).unwrap() else { panic!() };
    assert_eq!(changed.decode_stream(&old).unwrap(), form.as_bytes(), "the form itself is as it was");
    // Both pages: each gets a copy of its own.
    let all = replace_text(&doc, "", "Shared", "Own", None).unwrap();
    assert_eq!((all.count, read(&all.data, 0).0), (2, "Own text\nOwn text\n".to_string()));
    // A form that a page draws twice is left as it is (what is asked for is one of two places).
    let mut c = PdfBuilder::new();
    c.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    c.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    c.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Fm1 20 0 R >> >> >>");
    c.stream_obj(4, "", b"/Fm1 Do 1 0 0 1 0 -50 cm /Fm1 Do");
    c.obj(5, HELV);
    c.stream_obj(20, "/Type /XObject /Subtype /Form /BBox [0 0 300 100] /Resources << /Font << /F1 5 0 R >> >>", form.as_bytes());
    let doc = Document::from_bytes(c.finish_classic(21, "/Root 1 0 R")).unwrap();
    let none = replace_text(&doc, "", "Shared", "Own", None).unwrap();
    assert_eq!(none.count, 0, "{none:?}");
    assert!(none.skipped.iter().any(|(why, n)| why.contains("more than once") && *n == 2), "{:?}", none.skipped);
}

#[test]
fn a_line_is_changed_where_it_was_clicked_and_a_stale_or_bad_request_is_refused() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    let original = one_page("BT /F1 12 Tf 72 700 Td (Hello World today) Tj ET", HELV, "", &[]);
    let doc = Document::from_bytes(original).unwrap();
    let pages = doc.pages().unwrap();
    let line = text_line(&doc, &pages[0], [90.0, 88.0], &Work::new()).unwrap().unwrap();
    assert_eq!(line.text, "Hello World today");
    assert!(line.why.is_empty() && line.rect[0] < 80.0 && line.rect[2] > 130.0, "{line:?}");
    assert!(text_line(&doc, &pages[0], [90.0, 300.0], &Work::new()).unwrap().is_none(), "no text there");
    let edit = |old: &str, text: &str| Edit::ReplaceLine { page: 0, at: [90.0, 88.0], old: old.to_string(), text: text.to_string() };
    assert!(apply(&doc, &pages, &edit("Hello World today", "Hello Mars today"), NOW).is_ok());
    for (old, text, why) in [("Hello", "x", "stale"), ("Hello World today", "Hello World today", "same"), ("Hello World today", "a\nb", "line end")] {
        assert!(apply(&doc, &pages, &edit(old, text), NOW).is_err(), "{why}");
    }
    let objects = apply(&doc, &pages, &edit("Hello World today", "Hello Mars today"), NOW).unwrap();
    commit(&doc, objects);
    let section = save_section(&doc).unwrap();
    let mut bytes = doc.data().to_vec();
    bytes.extend_from_slice(&section);
    let (text, new) = read(&bytes, 0);
    assert_eq!(text, "Hello Mars today\n");
    // (The font has no /Widths, its spacing is a guess: nothing is made up for.)
    assert_eq!(new, "BT /F1 12 Tf 72 700 Td (Hello Mars today) Tj ET");
    // Only the middle of the line changes: a word added at the end, one taken out at the front.
    let edit2 = |old: &str, text: &str| Edit::ReplaceLine { page: 0, at: [90.0, 88.0], old: old.to_string(), text: text.to_string() };
    let doc = Document::from_bytes(bytes).unwrap();
    let pages = doc.pages().unwrap();
    for (text, shown) in [("Hello Mars today!", "(Hello Mars today!) Tj"), ("Mars today", "(Mars today) Tj"), ("Hello today", "(Hello today) Tj")] {
        let objects = apply(&doc, &pages, &edit2("Hello Mars today", text), NOW).unwrap();
        let copy = Document::from_bytes(doc.data().to_vec()).unwrap();
        commit(&copy, objects);
        let mut out = copy.data().to_vec();
        out.extend_from_slice(&save_section(&copy).unwrap());
        let (got, new) = read(&out, 0);
        assert_eq!(got, format!("{text}\n"));
        assert!(new.contains(shown), "{new}");
    }
}

#[test]
fn what_is_too_much_is_refused_while_it_is_made() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    // More places than a page may have changes.
    let many = format!("BT /F1 12 Tf 72 700 Td ({}) Tj ET", "a".repeat(MAX_HITS_PER_PAGE + 10));
    let doc = Document::from_bytes(one_page(&many, HELV, "", &[])).unwrap();
    let err = replace_text(&doc, "", "a", "b", None).unwrap_err();
    assert!(matches!(err, Error::Limit(_)), "{err:?}");
    // A work meter that is nearly spent, and room for almost nothing.
    let doc = Document::from_bytes(one_page("BT /F1 12 Tf 72 700 Td (Hello World) Tj ET", HELV, "", &[])).unwrap();
    let pages = doc.pages().unwrap();
    let edit = Edit::ReplaceLine { page: 0, at: [90.0, 88.0], old: "Hello World".to_string(), text: "Hello Mars".to_string() };
    let err = apply_metered(&doc, &pages, &edit, NOW, &Work::with_allowance(50_000.0), DEFAULT_ROOM).unwrap_err();
    assert!(matches!(err, Error::Limit(_)), "{err:?}");
    let err = apply_metered(&doc, &pages, &edit, NOW, &Work::new(), 100).unwrap_err();
    assert!(matches!(&err, Error::Limit(m) if m == TOO_BIG), "{err:?}");
    assert!(apply_metered(&doc, &pages, &edit, NOW, &Work::new(), DEFAULT_ROOM).is_ok());
    // A form that draws itself and a font that is not there do not stop the page from being read.
    let mut c = PdfBuilder::new();
    c.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    c.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    c.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 20 0 R >> >> >>");
    c.stream_obj(4, "", b"/Fm1 Do BT /Nope 12 Tf (Lost) Tj /F1 12 Tf 72 700 Td (Hello) Tj ET");
    c.obj(5, HELV);
    c.stream_obj(20, "/Type /XObject /Subtype /Form /BBox [0 0 300 100] /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 20 0 R >> >>", b"/Fm1 Do");
    let doc = Document::from_bytes(c.finish_classic(21, "/Root 1 0 R")).unwrap();
    assert_eq!(replace_text(&doc, "", "Hello", "Bye", None).unwrap().count, 1);
}

#[test]
fn text_in_a_type_3_font_is_replaced_by_text_in_a_type_3_font_with_the_same_matrix() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    // A font of one glyph, `a`, drawn upside down by its matrix and turned upright by the text matrix, as Chrome writes them.
    let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 -800 1000 200] /FontMatrix [0.001 0 0 -0.001 0 0] /CharProcs << /a 6 0 R >> \
                /Encoding << /Type /Encoding /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [600] /ToUnicode 7 0 R /Resources << >> >>";
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"BT /F1 20 Tf 1 0 0 -1 72 700 Tm (aa) Tj ET");
    b.obj(5, font);
    b.stream_obj(6, "", b"600 0 0 -700 500 0 d1 0 0 500 -700 re f");
    b.stream_obj(7, "", b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <61> <0061> endbfchar");
    let original = b.finish_classic(8, "/Root 1 0 R");
    let doc = Document::from_bytes(original).unwrap();
    // Letters the font has are shown by it; the others by a Type 3 font made like it.
    let same = replace_text(&doc, "", "aa", "aaa", None).unwrap();
    assert_eq!(same.count, 1);
    assert!(read(&same.data, 0).1.contains("(aaa) Tj") && !read(&same.data, 0).1.contains("Qf"));
    let done = replace_text(&doc, "", "aa", "xy", None).unwrap();
    assert_eq!(done.count, 1);
    let (text, new) = read(&done.data, 0);
    assert_eq!(text, "xy\n");
    assert!(new.contains("/Qf") && new.contains(" 20 Tf"), "{new}");
    let changed = Document::from_bytes(done.data).unwrap();
    let page = &changed.pages().unwrap()[0];
    let Object::Dict(res) = changed.resolve(page.resources().unwrap()).unwrap() else { panic!() };
    let Some(Object::Dict(fonts)) = res.get("Font") else { panic!() };
    let (_, made) = fonts.iter().find(|(k, _)| k.0.starts_with(b"Qf")).unwrap();
    let Object::Dict(made) = changed.resolve(made).unwrap() else { panic!() };
    assert_eq!(made.get_name("Subtype").map(|n| n.as_bytes().to_vec()), Some(b"Type3".to_vec()));
    let matrix: Vec<f64> = made.get("FontMatrix").and_then(Object::as_array).unwrap().iter().filter_map(Object::as_f64).collect();
    assert_eq!(matrix, vec![0.001, 0.0, 0.0, -0.001, 0.0, 0.0]);
}

#[test]
fn odd_pages_are_changed_where_they_can_be_refused_where_they_cannot_and_never_break() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    let file = |contents: &str, streams: &[(u32, &str)]| {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {contents} /Resources << /Font << /F1 5 0 R >> >> >>"));
        b.obj(5, HELV);
        for (n, data) in streams {
            b.stream_obj(*n, "", data.as_bytes());
        }
        Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap()
    };
    // A statement that begins in one stream and ends in the next (7.8.2 allows it) cannot be written back into one of them.
    let split = file("[4 0 R 6 0 R]", &[(4, "BT /F1 12 Tf 72 700 Td [(Hel"), (6, "lo) -300 (World)] TJ ET")]);
    let done = replace_text(&split, "", "World", "Mars", None).unwrap();
    assert_eq!(done.count, 0, "{:?}", done.skipped);
    assert!(done.skipped.iter().any(|(why, _)| why.contains("content stream")), "{:?}", done.skipped);
    // The same stream twice in one page's list: the text of it is shown twice, and neither can be told from the other.
    let twice = file("[4 0 R 4 0 R]", &[(4, "BT /F1 12 Tf 72 700 Td (Hello World) Tj ET")]);
    let done = replace_text(&twice, "", "World", "Mars", None).unwrap();
    assert_eq!(done.count, 0);
    assert!(done.skipped.iter().any(|(why, _)| why.contains("twice")), "{:?}", done.skipped);
    // A stream that is cut off in the middle of a string, a page with no content at all, and a content stream that is not text.
    let cut = file("4 0 R", &[(4, "BT /F1 12 Tf (Hello) Tj (never closed")]);
    assert_eq!(replace_text(&cut, "", "Hello", "Bye", None).unwrap().count, 1);
    assert_eq!(replace_text(&file("[]", &[]), "", "Hello", "Bye", None).unwrap().count, 0);
    assert_eq!(replace_text(&file("4 0 R", &[(4, "  ( ) [ << >> ] Tj TJ ' \" BT BT ET")]), "", "a", "b", None).unwrap().count, 0);
    // A Type 3 font whose matrix cannot be turned over is not turned over: the change is refused, not made wrong.
    if have_chinese_font() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
        b.stream_obj(4, "", b"BT /F1 20 Tf 72 700 Td (a) Tj ET");
        b.obj(5, "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [0 0 0 0 0 0] /CharProcs << /a 6 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [1] /ToUnicode 7 0 R >>");
        b.stream_obj(6, "", b"1 0 0 0 1 1 d1");
        b.stream_obj(7, "", b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <61> <0061> endbfchar");
        let doc = Document::from_bytes(b.finish_classic(8, "/Root 1 0 R")).unwrap();
        let done = replace_text(&doc, "", "a", "你", None).unwrap();
        assert_eq!(done.count, 0);
        assert!(done.skipped.iter().any(|(why, _)| why.contains("font matrix")), "{:?}", done.skipped);
    }
}

#[test]
fn the_work_meter_is_charged_about_what_the_work_takes() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    // 1500 pages of 30 pieces of text each, every one with a word to change.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..1_500).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 1500 /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> >>"));
    b.obj(5, HELV);
    let text: String = (0..30).map(|k| format!("0 -20 Td [(The) -250 (quick) -250 (brown) -250 (fox) -250 (jumps {k})] TJ
")).collect();
    for i in 0..1_500u32 {
        b.obj(10 + 2 * i, &format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i));
        b.stream_obj(11 + 2 * i, "", format!("BT /F1 12 Tf 72 780 Td
{text}ET").as_bytes());
    }
    let doc = Document::from_bytes(b.finish_classic(10 + 3_000, "/Root 1 0 R")).unwrap();
    let started = std::time::Instant::now();
    let done = replace_text(&doc, "", "quick brown", "slow red", None).unwrap();
    let took = started.elapsed().as_secs_f64() * 1.0e9;
    assert_eq!((done.count, done.pages), (45_000, 1_500));
    let ratio = done.work_used / took;
    println!("{} hits in {:.0} ms: the meter was charged {:.2e} units, {ratio:.2} of the time", done.count, took / 1.0e6, done.work_used);
    assert!((0.3..8.0).contains(&ratio), "the meter says {:.2e} units for {:.2e} ns", done.work_used, took);
}

/// Where the glyphs of the first page are: the character and the x of its start, in the order they are drawn.
fn glyph_xs(bytes: &[u8]) -> Vec<(char, f64)> {
    let doc = Document::from_bytes(bytes.to_vec()).unwrap();
    let pages = doc.pages().unwrap();
    let mut scanner = crate::text::EditScanner::new(&doc);
    let scan = scanner.scan(&pages[0], &Work::new()).unwrap();
    scan.glyphs.iter().map(|g| (g.uni.first().unwrap_or('?'), g.x)).collect()
}

// --- the findings of the review of 5a ----------------------------------------------------------------------------

#[test]
fn an_actual_text_over_the_old_text_is_rewritten_when_it_holds_only_that_and_taken_out_otherwise() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    let content = [
        "/Span << /ActualText (secretword) /MCID 3 >> BDC BT /F1 12 Tf 72 700 Td (secretword) Tj ET EMC",
        "/Span << /ActualText (Hello World) /Lang (en) >> BDC BT /F1 12 Tf 72 650 Td (Hello World) Tj ET EMC",
        "/P << /ActualText (Hello World) >> BDC BT /F1 12 Tf 72 600 Td (Hello World) Tj ET EMC",
        "/Span << /ActualText (not the same) >> BDC /Span << /ActualText (Hello World again) >> BDC BT /F1 12 Tf 72 550 Td (Hello World again) Tj ET EMC EMC",
    ]
    .join("\n");
    let doc = Document::from_bytes(one_page(&content, HELV, "", &[])).unwrap();
    // The sequence that holds the word and nothing else gets the new word (and keeps its other entries).
    let done = replace_text(&doc, "", "secretword", "publicword", None).unwrap();
    assert_eq!(done.count, 1, "{:?}", done.skipped);
    let all = read(&done.data, 0).1;
    assert!(!all.contains("secretword"), "{all}");
    assert_eq!(all.matches("publicword").count(), 2, "once in the ActualText, once drawn: {all}");
    assert!(all.contains("MCID"), "{all}");
    // Sequences that hold more lose the entry (a sequence left with no entry is a BMC); the other entries stay; the ones around
    // a hit in a sequence inside another lose theirs too.
    let done = replace_text(&doc, "", "World", "Mars", None).unwrap();
    assert_eq!(done.count, 3, "{:?}", done.skipped);
    let all = read(&done.data, 0).1;
    assert!(!all.contains("Hello World"), "{all}");
    assert!(all.contains("/P BMC") && all.contains("Lang") && !all.contains("not the same"), "{all}");
    assert_eq!(all.matches("Hello Mars").count(), 3, "{all}");
    // Whatever is left of the words extracts as the new text only.
    assert_eq!(read(&done.data, 0).0, "secretword\nHello Mars\nHello Mars\nHello Mars again\n");
}

/// Helvetica with its widths written in the font (a standard font may come without; its spacing is then a guess).
fn helvetica_with_widths() -> String {
    const WIDTHS: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
        1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
        333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
    ];
    let list: Vec<String> = WIDTHS.iter().map(u16::to_string).collect();
    format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{}] >>", list.join(" "))
}

#[test]
fn what_follows_the_replaced_text_in_the_operator_stays_where_it_was() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    // Wider text, narrower text, with scaling, character spacing and word spacing in force, in an array and in a string.
    let content = [
        "BT /F1 12 Tf 72 700 Td [(He) -500 (llo) 300 (World)] TJ ET",
        "BT /F1 12 Tf 50 Tz 1 Tc 3 Tw 72 650 Td (Hello big World) Tj ET",
        "BT /F1 10 Tf 14 TL 72 600 Td (first) Tj (the llo World) ' ET",
    ]
    .join("\n");
    let original = one_page(&content, &helvetica_with_widths(), "", &[]);
    let doc = Document::from_bytes(original.clone()).unwrap();
    let w = |g: &[(char, f64)]| g.iter().filter(|e| e.0 == 'W').map(|e| e.1).collect::<Vec<_>>();
    assert_eq!(w(&glyph_xs(&original)).len(), 3);
    for with in ["XY", "Qwertzuiop", ""] {
        let done = replace_text(&doc, "", "llo", with, None).unwrap();
        assert_eq!(done.count, 3, "{:?}", done.skipped);
        let (before, after) = (w(&glyph_xs(&original)), w(&glyph_xs(&done.data)));
        assert_eq!(after.len(), 3, "{with}: {after:?}");
        // Every 'W' of "World" is where it was, whatever stands before it.
        for (a, b) in before.iter().zip(&after) {
            assert!((a - b).abs() < 0.01, "{with}: World moved from {a} to {b}");
        }
    }
    // A font without /Widths has a guess for the spacing, which is not the one other readers use: nothing is made up for there.
    let guessed = Document::from_bytes(one_page(&content, HELV, "", &[])).unwrap();
    let done = replace_text(&guessed, "", "llo", "XY", None).unwrap();
    assert!(read(&done.data, 0).1.contains("[(He) -500 (XY) 300 (World)] TJ"), "{}", read(&done.data, 0).1);
    // Nothing follows the last word of a line in its operator: nothing is made up for.
    let done = replace_text(&doc, "", "World", "Worlds", None).unwrap();
    assert_eq!(done.count, 3);
    let content = read(&done.data, 0).1;
    assert!(content.contains("(Hello big Worlds) Tj") && content.contains("(the llo Worlds) Tj"), "{content}");
}

#[test]
fn many_content_streams_and_empty_strings_do_not_take_time_or_memory_the_page_does_not_have() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    // A page of 40000 content streams of one letter each, 20000 of them to be changed: found by position, not by looking through
    // the list for every character.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    let streams = 40_000u32;
    let list: String = (0..streams).map(|i| format!("{} 0 R ", 10 + i)).collect();
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [{list}] /Resources << /Font << /F1 5 0 R >> >> >>"));
    b.obj(5, HELV);
    for i in 0..streams {
        b.stream_obj(10 + i, "", format!("BT /F1 12 Tf {} {} Td ({}) Tj ET", 20 + (i % 40) * 14, 780 - (i / 40) * 2, if i % 2 == 0 { 'e' } else { 'x' }).as_bytes());
    }
    let doc = Document::from_bytes(b.finish_classic(10 + streams, "/Root 1 0 R")).unwrap();
    let started = std::time::Instant::now();
    let done = replace_text(&doc, "", "e", "X", None).unwrap();
    let took = started.elapsed().as_secs_f64();
    assert_eq!(done.count, 20_000, "{:?}", done.skipped);
    assert!(took < 4.0, "{took} s for {streams} content streams (the looking through the list took 5 s and more)");
    // Millions of empty strings are nothing to keep: the page is read into what is on it.
    let empties = "() Tj ".repeat(3_000_000);
    let page = one_page(&format!("BT /F1 12 Tf {empties} 72 700 Td (zzz) Tj ET"), HELV, "", &[]);
    let doc = Document::from_bytes(page).unwrap();
    let pages = doc.pages().unwrap();
    let started = std::time::Instant::now();
    let scan = crate::text::EditScanner::new(&doc).scan(&pages[0], &Work::new()).unwrap();
    assert_eq!((scan.shows.len(), scan.parts.len(), scan.glyphs.len()), (1, 1, 3));
    assert!(started.elapsed().as_secs_f64() < 4.0);
    // And a page with more pieces than may be kept is refused, not read.
    let many = "(a) Tj ".repeat(700_000);
    let doc = Document::from_bytes(one_page(&format!("BT /F1 12 Tf {many} ET"), HELV, "", &[])).unwrap();
    let pages = doc.pages().unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(crate::text::EditScanner::new(&doc).scan(&pages[0], &Work::new()), Err(Error::Limit(_))));
    assert!(started.elapsed().as_secs_f64() < 4.0);
}

#[test]
fn the_new_text_is_charged_for_by_its_characters_and_a_font_that_cannot_be_made_skips_the_hit() {
    if !have_chinese_font() {
        println!("SKIPPED: no Chinese font on this machine");
        return;
    }
    // The same hits with a text of 1 and of 300 characters: the meter says how much more work the longer one is.
    let items: String = (0..2_000).map(|i| format!("BT /F1 12 Tf {} {} Td (e) Tj ET\n", 20 + (i % 40) * 14, 780 - (i / 40) * 2)).collect();
    let doc = Document::from_bytes(one_page(&items, HELV, "", &[])).unwrap();
    let long: String = (0..300u32).map(|i| char::from_u32(0x4E00 + i).unwrap()).collect();
    let (short_run, long_run) = (replace_text(&doc, "", "e", "X", None).unwrap(), replace_text(&doc, "", "e", &long, None).unwrap());
    assert_eq!((short_run.count, long_run.count), (2_000, 2_000));
    assert!(long_run.work_used - short_run.work_used > 2_000.0 * 299.0 * 50.0, "{} against {}", long_run.work_used, short_run.work_used);
    // 20000 hits of 300 different characters each: in seconds, not in the minutes that looking up each character in a list takes.
    let items: String = (0..20_000).map(|i| format!("BT /F1 12 Tf {} {} Td (e) Tj ET\n", 20 + (i % 40) * 14, 780 - (i / 40) * 2)).collect();
    let doc = Document::from_bytes(one_page(&items, HELV, "", &[])).unwrap();
    let started = std::time::Instant::now();
    let done = replace_text(&doc, "", "e", &long, None).unwrap();
    assert_eq!(done.count, 20_000);
    assert!(started.elapsed().as_secs_f64() < 6.0, "{} s", started.elapsed().as_secs_f64());
    // A Type 3 font that cannot take the text (more than 255 characters it lacks): that hit is skipped with the reason and the rest
    // of the page is changed. Only the characters it lacks count (it has an `a`).
    let font3 = "<< /Type /Font /Subtype /Type3 /FontBBox [0 -800 1000 200] /FontMatrix [0.001 0 0 -0.001 0 0] /CharProcs << /a 6 0 R >> /Encoding << /Type /Encoding /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [600] /ToUnicode 7 0 R /Resources << >> >>";
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 8 0 R >> >> >>");
    b.stream_obj(4, "", b"BT /F1 20 Tf 1 0 0 -1 72 700 Tm (aa) Tj ET BT /F2 12 Tf 72 600 Td (za) Tj ET");
    b.obj(5, font3);
    b.stream_obj(6, "", b"600 0 0 -700 500 0 d1 0 0 500 -700 re f");
    b.stream_obj(7, "", b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <61> <0061> endbfchar");
    b.obj(8, HELV);
    let doc = Document::from_bytes(b.finish_classic(9, "/Root 1 0 R")).unwrap();
    let too_many: String = (0..300u32).map(|i| char::from_u32(0x4E00 + i).unwrap()).collect();
    let done = replace_text(&doc, "", "a", &too_many, None).unwrap();
    assert_eq!(done.count, 1, "the line in Helvetica is changed, the Type 3 ones are skipped: {:?}", done.skipped);
    assert!(done.skipped.iter().any(|(why, _)| why.contains("255")), "{:?}", done.skipped);
    let fits: String = std::iter::once('a').chain((0..255u32).map(|i| char::from_u32(0x4E00 + i).unwrap())).collect();
    let done = replace_text(&doc, "", "a", &fits, None).unwrap();
    assert_eq!(done.count, 3, "255 characters the font lacks are enough: {:?}", done.skipped);
}

#[test]
fn places_that_cannot_be_changed_count_toward_the_limit_and_a_form_that_many_pages_share_is_copied_once() {
    if !have_latin_font() {
        println!("SKIPPED: no Latin font on this machine");
        return;
    }
    // A glyph that stands for two characters ("ee"): every "e" found is cut by it and cannot be changed, but is met all the same.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(4, "", format!("BT /F1 12 Tf 72 700 Td ({}) Tj ET", "A".repeat(MAX_HITS_PER_PAGE + 10)).as_bytes());
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>");
    b.stream_obj(6, "", b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <00650065> endbfchar");
    let doc = Document::from_bytes(b.finish_classic(7, "/Root 1 0 R")).unwrap();
    assert!(matches!(replace_text(&doc, "", "e", "x", None), Err(Error::Limit(_))));
    // 50 pages that draw one form, a header with a word to change: one copy of the form, which every page draws.
    let pages = 50u32;
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 612 792] >>"));
    b.obj(5, HELV);
    b.stream_obj(6, "/Type /XObject /Subtype /Form /BBox [0 0 600 60] /Resources << /Font << /F1 5 0 R >> >>", format!("BT /F1 12 Tf 10 10 Td (secret llo header) Tj ET\n%{}\n", "x".repeat(20_000)).as_bytes());
    for i in 0..pages {
        b.obj(10 + 2 * i, &format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Fm0 6 0 R >> >> >>", 11 + 2 * i));
        b.stream_obj(11 + 2 * i, "", b"q 1 0 0 1 0 700 cm /Fm0 Do Q");
    }
    let original = b.finish_classic(10 + 2 * pages, "/Root 1 0 R");
    let doc = Document::from_bytes(original.clone()).unwrap();
    let done = replace_text(&doc, "", "llo", "XY", None).unwrap();
    assert_eq!((done.count, done.pages), (pages as usize, pages as usize), "{:?}", done.skipped);
    assert!(done.data.len() < original.len() + 60_000, "{} bytes more for a form of 20000: it was copied for every page", done.data.len() - original.len());
    let changed = Document::from_bytes(done.data).unwrap();
    let mut copies = std::collections::BTreeSet::new();
    for page in changed.pages().unwrap() {
        let Object::Dict(res) = changed.resolve(page.resources().unwrap()).unwrap() else { panic!() };
        let Some(Object::Dict(forms)) = res.get("XObject") else { panic!() };
        copies.insert(format!("{:?}", forms.get("Fm0").unwrap()));
    }
    assert_eq!(copies.len(), 1, "{copies:?}");
}
