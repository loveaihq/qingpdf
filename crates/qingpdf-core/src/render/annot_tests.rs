//! Step 3c2-3: annotation appearances (ISO 32000-1 12.5), drawn on a 100 by 100 point page at 72 dpi (one pixel a
//! point; rows from the top, so the user point (x, y) is the pixel (x, 99 - y)), and the hostile files that go with
//! them: every one is built here, in code.

use std::time::{Duration, Instant};

use super::tests::{WHITE, pixel};
use super::work::{PAGE_WORK, cost};
use super::*;
use crate::testutil::PdfBuilder;

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// A page with these entries (a catalog entry, a resource dictionary, extra page entries), this content, and further
/// objects and streams.
fn doc_of(
    catalog_extra: &str,
    resources: &str,
    page_extra: &str,
    content: &str,
    objs: &[(u32, String)],
    streams: &[(u32, String, Vec<u8>)],
) -> Document {
    let mut b = PdfBuilder::new();
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"));
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources {resources} /Contents 4 0 R {page_extra} >>"));
    b.stream_obj(4, "", content.as_bytes());
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

/// A form XObject: object `n`, with this bounding box and further entries.
fn form(n: u32, entries: &str, content: &str) -> (u32, String, Vec<u8>) {
    (n, format!("/Type /XObject /Subtype /Form {entries}"), content.as_bytes().to_vec())
}

/// Objects 5 (a red square of 10 points) and 6 (a green one).
fn squares() -> Vec<(u32, String, Vec<u8>)> {
    vec![form(5, "/BBox [0 0 10 10]", "1 0 0 rg 0 0 10 10 re f"), form(6, "/BBox [0 0 10 10]", "0 1 0 rg 0 0 10 10 re f")]
}

/// An annotation dictionary at `x`, `y` (10 points square) with the extra entries.
fn annot(x: u32, y: u32, extra: &str) -> String {
    format!("<< /Type /Annot /Subtype /Square /Rect [{x} {y} {} {}] {extra} >>", x + 10, y + 10)
}

const AP_RED: &str = "/AP << /N 5 0 R >>";

/// Draw the first page with the annotations on (or off).
fn render(doc: &Document, annotations: bool) -> (Bitmap, Vec<String>) {
    let mut r = Renderer::new(doc);
    r.set_annotations(annotations);
    let pages = doc.pages().expect("pages");
    let bitmap = r.render_page(&pages[0], 72.0).expect("renders");
    (bitmap, r.take_warnings())
}

fn has(warnings: &[String], text: &str) -> bool {
    warnings.iter().any(|w| w.contains(text))
}

/// The pixel at the middle of the square annotation at (x, y).
fn centre(b: &Bitmap, x: u32, y: u32) -> [u8; 3] {
    pixel(b, x + 5, 99 - (y + 5))
}

#[test]
fn an_appearance_is_put_on_the_rectangle() {
    // A 10 point square form on a rectangle 40 by 20: scaled, and moved to the rectangle. Drawn on top of the page.
    let objs = [(10, "<< /Type /Annot /Subtype /Square /Rect [20 20 60 40] /AP << /N 5 0 R >> >>".to_string())];
    let doc = doc_of("", "<< >>", "/Annots [10 0 R]", "0 0 1 rg 0 0 100 100 re f", &objs, &squares());
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 30, 70), RED);
    assert_eq!(pixel(&b, 58, 62), RED);
    assert_eq!(pixel(&b, 62, 70), BLUE);
    assert_eq!(pixel(&b, 30, 50), BLUE);
    assert_eq!(pixel(&b, 30, 85), BLUE);
    // Without annotations the page is the page.
    let (b, _) = render(&doc, false);
    assert_eq!(pixel(&b, 30, 70), BLUE);
}

#[test]
fn the_form_matrix_and_the_page_rotation_are_taken_into_account() {
    // 12.5.5 Algorithm 8.1. The form is 10 by 20 with a turn by 90 degrees: its box becomes 20 wide and 10 high (x from -20
    // to 0, y from 0 to 10), which is scaled by 2 onto the rectangle [10 10 50 30]. Its left half (red) is the lower half
    // after the turn.
    let objs = [(10, "<< /Type /Annot /Subtype /Stamp /Rect [10 10 50 30] /AP << /N 5 0 R >> >>".to_string())];
    let streams = [form(5, "/BBox [0 0 10 20] /Matrix [0 1 -1 0 0 0]", "1 0 0 rg 0 0 5 20 re f 0 0 1 rg 5 0 5 20 re f")];
    let doc = doc_of("", "<< >>", "/Annots [10 0 R]", "", &objs, &streams);
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 30, 85), RED);
    assert_eq!(pixel(&b, 30, 75), BLUE);
    assert_eq!(pixel(&b, 30, 65), WHITE);
    assert_eq!(pixel(&b, 5, 85), WHITE);
    // A form without resources of its own draws with the page's.
    let objs = [(10, annot(40, 40, AP_RED).replace("5 0 R", "7 0 R"))];
    let mut streams = squares();
    streams.push(form(7, "/BBox [0 0 10 10]", "/Inner Do"));
    let doc = doc_of("", "<< /XObject << /Inner 5 0 R >> >>", "/Annots [10 0 R]", "", &objs, &streams);
    assert_eq!(centre(&render(&doc, true).0, 40, 40), RED);
    // The page turned by 90 degrees: the lower left corner is at the top left.
    let objs = [(10, "<< /Type /Annot /Subtype /Square /Rect [0 0 20 10] /AP << /N 5 0 R >> >>".to_string())];
    let streams = [form(5, "/BBox [0 0 20 10]", "1 0 0 rg 0 0 20 10 re f")];
    let doc = doc_of("", "<< >>", "/Annots [10 0 R] /Rotate 90", "", &objs, &streams);
    let (b, _) = render(&doc, true);
    assert_eq!(pixel(&b, 5, 10), RED);
    assert_eq!(pixel(&b, 5, 40), WHITE);
}

#[test]
fn states_are_chosen_by_as_or_by_the_value() {
    // 12.5.5: /AS names the state. Without it the value of the field (here or above) names it when the dictionary has that
    // state, else it is `Off`. A state that is not there draws nothing.
    let states = "/AP << /N << /On 5 0 R /Off 6 0 R >> >>";
    let cases = [
        (0, 80, format!("/AS /On {states}"), RED),
        (20, 80, format!("/AS /Off {states}"), GREEN),
        (40, 80, format!("/AS /Nope {states}"), WHITE),
        (60, 80, format!("/V /On {states}"), RED),
        (80, 80, format!("/Parent 30 0 R {states}"), RED),
        (0, 50, states.to_string(), GREEN),
        (20, 50, format!("/V /Nope {states}"), GREEN),
        (40, 50, format!("/Parent 31 0 R {states}"), GREEN),
        (60, 50, format!("/AS 5 {states}"), WHITE),
        (80, 50, "/AS /On /AP << /N << /On 99 0 R >> >>".to_string(), WHITE),
    ];
    let mut objs: Vec<(u32, String)> = cases.iter().enumerate().map(|(i, (x, y, extra, _))| (10 + i as u32, annot(*x, *y, extra))).collect();
    objs.push((30, "<< /V /On >>".to_string()));
    objs.push((31, "<< /Parent 32 0 R >>".to_string()));
    objs.push((32, "<< /Parent 31 0 R >>".to_string()));
    let list: String = (0..cases.len()).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &squares());
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    for (x, y, extra, want) in &cases {
        assert_eq!(centre(&b, *x, *y), *want, "{extra}");
    }
}

#[test]
fn hidden_annotations_popups_and_optional_content() {
    // 12.5.3: Hidden and NoView are not drawn; Invisible only hides the annotations of a type that is not a standard one;
    // Print alone changes nothing. A popup is not drawn. An annotation whose /OC is off, or whose appearance stream's is,
    // is not drawn.
    let catalog = "/OCProperties << /OCGs [40 0 R 41 0 R] /D << /OFF [40 0 R] >> >>";
    let cases = [
        (0, 80, "/F 2".to_string(), WHITE),
        (20, 80, "/F 32".to_string(), WHITE),
        (40, 80, "/Subtype /Popup".to_string(), WHITE),
        (60, 80, "/F 1 /Subtype /Custom".to_string(), WHITE),
        (80, 80, "/F 1".to_string(), RED),
        (0, 50, "/F 4".to_string(), RED),
        (20, 50, "/Subtype /Link".to_string(), RED),
        (40, 50, "/OC 40 0 R".to_string(), WHITE),
        (60, 50, "/OC 41 0 R".to_string(), RED),
        (80, 50, "/AP << /N 7 0 R >>".to_string(), WHITE),
    ];
    // (The later /Subtype wins no more than the first: a dictionary keeps one.)
    let mut objs: Vec<(u32, String)> = cases
        .iter()
        .enumerate()
        .map(|(i, (x, y, extra, _))| {
            let body = if extra.contains("/Subtype") { annot(*x, *y, &format!("{AP_RED} {extra}")).replace("/Subtype /Square", "") } else { annot(*x, *y, &format!("{AP_RED} {extra}")) };
            (10 + i as u32, body)
        })
        .collect();
    objs.push((40, "<< /Type /OCG /Name (off) >>".to_string()));
    objs.push((41, "<< /Type /OCG /Name (on) >>".to_string()));
    let mut streams = squares();
    streams.push(form(7, "/BBox [0 0 10 10] /OC 40 0 R", "1 0 0 rg 0 0 10 10 re f"));
    let list: String = (0..cases.len()).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let doc = doc_of(catalog, "<< >>", &format!("/Annots [{list}]"), "", &objs, &streams);
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    // (The last case has two /AP entries; the later one is what the dictionary keeps.)
    for (x, y, extra, want) in &cases {
        assert_eq!(centre(&b, *x, *y), *want, "{extra}");
    }
}

#[test]
fn form_fields_without_an_appearance_are_skipped_with_a_warning() {
    let objs = [
        (10, "<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] /FT /Tx >>".to_string()),
        (11, "<< /Type /Annot /Subtype /Widget /Rect [20 0 30 10] /AP << /D 5 0 R >> >>".to_string()),
        (12, "<< /Type /Annot /Subtype /Link /Rect [40 0 50 10] >>".to_string()),
        (13, "<< /Type /Annot /Subtype /Widget /Rect [60 0 70 10] /F 2 >>".to_string()),
    ];
    let doc = doc_of("", "<< >>", "/Annots [10 0 R 11 0 R 12 0 R 13 0 R]", "", &objs, &squares());
    let (_, w) = render(&doc, true);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(has(&w, "form field has no appearance stream"), "{w:?}");
    // No annotation at all: nothing to say.
    let doc = doc_of("", "<< >>", "", "0 g 0 0 5 5 re f", &[], &[]);
    assert!(render(&doc, true).1.is_empty());
}

#[test]
fn annotations_start_from_a_clean_state() {
    // The page's content ends inside a clip, with unbalanced `q`s and an open optional content section that is off: none of it
    // reaches the annotations.
    let catalog = "/OCProperties << /OCGs [40 0 R] /D << /OFF [40 0 R] >> >>";
    let res = "<< /Properties << /Off 40 0 R >> >>";
    let content = "q q q 0 0 5 5 re W n /OC /Off BDC /Span BMC q 1 0 0 1 50 50 cm";
    let objs = [(10, annot(50, 50, AP_RED)), (40, "<< /Type /OCG /Name (off) >>".to_string())];
    let doc = doc_of(catalog, res, "/Annots [10 0 R]", content, &objs, &squares());
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 50, 50), RED);
}

// --- hostile files ---------------------------------------------------------------------------------------------

/// Draw the first page and say that it took less than `seconds`. Also the work the page used.
fn quick(doc: &Document, seconds: u64) -> (Bitmap, Vec<String>, f64, (usize, usize, usize)) {
    let start = Instant::now();
    let mut r = Renderer::new(doc);
    let pages = doc.pages().expect("pages");
    let bitmap = r.render_page(&pages[0], 72.0).expect("renders");
    let taken = start.elapsed();
    assert!(taken < Duration::from_secs(seconds), "took {taken:?}");
    (bitmap, r.take_warnings(), r.last_work_for_test(), r.object_cache_for_test())
}

#[test]
fn ten_thousand_annotations_share_one_appearance_read_once() {
    let n = 10_000u32;
    let objs: Vec<(u32, String)> = (0..n).map(|i| (10 + i, annot((i * 7) % 90, (i * 13) % 90, AP_RED))).collect();
    let list: String = (0..n).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let streams = squares();
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &streams);
    let (b, w, work, (form_bytes, _, _)) = quick(&doc, 10);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 0, 0), RED);
    // The stream was decoded and kept once; every annotation was charged for its visit and its drawing.
    assert_eq!(form_bytes, streams[0].2.len());
    assert!(work >= f64::from(n) * (cost::ANNOT_VISIT + cost::ANNOT_DRAW), "{work}");
    assert!(work < PAGE_WORK / 4.0, "{work}");
}

#[test]
fn more_than_the_most_annotations_are_left_out_and_hidden_ones_are_charged() {
    // 25,000 entries naming one hidden annotation: the first 20,000 are looked at (and paid for), the rest are not.
    let n = interp::MAX_ANNOTS + 5000;
    let objs = [(10, annot(10, 10, &format!("{AP_RED} /F 2")))];
    let list = "10 0 R ".repeat(n);
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &squares());
    let (b, w, work, _) = quick(&doc, 10);
    assert!(has(&w, "annotations; the rest are not drawn"), "{w:?}");
    assert_eq!(centre(&b, 10, 10), WHITE);
    assert!(work >= interp::MAX_ANNOTS as f64 * cost::ANNOT_VISIT, "{work}");
    // Off the page they cost the same.
    let objs = [(10, "<< /Type /Annot /Subtype /Square /Rect [500 500 510 510] /AP << /N 5 0 R >> >>".to_string())];
    let doc = doc_of("", "<< >>", &format!("/Annots [{}]", "10 0 R ".repeat(1000)), "", &objs, &squares());
    let (_, _, work, _) = quick(&doc, 10);
    assert!(work >= 1000.0 * cost::ANNOT_VISIT, "{work}");
}

#[test]
fn a_huge_dictionary_of_states_is_paid_for() {
    // An appearance dictionary with 100,000 states, shared by 3000 annotations that each look one up: the copying
    // and the search are charged for, so the page runs out of work and stops instead of taking minutes.
    let states: String = (0..100_000).map(|i| format!("/s{i} 5 0 R ")).collect();
    let mut objs = vec![(8, format!("<< {states} >>")), (7, "<< /N 8 0 R >>".to_string())];
    let count = 3000u32;
    for i in 0..count {
        objs.push((10 + i, annot((i * 7) % 90, (i * 13) % 90, "/AS /s99999 /AP 7 0 R")));
    }
    let list: String = (0..count).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &squares());
    let (_, w, work, _) = quick(&doc, 10);
    assert!(has(&w, "more work"), "{w:?}");
    assert!(work <= PAGE_WORK * 1.01, "{work}");
}

#[test]
fn rectangles_boxes_and_matrices_that_make_no_sense() {
    let nines = "9".repeat(400);
    let rects = [
        format!("[0 0 {nines} 10]"),
        "[0 0 0 10]".to_string(),
        "[-1000000000000000000000000000000 -1000000000000000000000000000000 1000000000000000000000000000000 1000000000000000000000000000000]".to_string(),
        "[-99999999999999999999999999999999999999 -99999999999999999999999999999999999999 99999999999999999999999999999999999999 99999999999999999999999999999999999999]".to_string(),
        "[1 2 3]".to_string(),
        "[/a /b /c /d]".to_string(),
        "7 0 R".to_string(),
        "[10 10 20 20]".to_string(),
    ];
    let forms = [
        "/BBox [0 0 0 0]",
        "",
        "/BBox [0 0 10 10] /Matrix [0 0 0 0 0 0]",
        "/BBox [0 0 10 10] /Matrix [99999999999999999999999999999999999999 0 0 99999999999999999999999999999999999999 0 0]",
        "/BBox [0 0 99999999999999999999999999999999999999 99999999999999999999999999999999999999]",
        "/BBox [0 0 10 10] /Matrix [1 0 0]",
        "/BBox [5 5 5 100]",
        "/BBox [0 0 10 10] /Matrix [1 0 0 /x 0 0]",
    ];
    let mut objs = Vec::new();
    let mut streams = Vec::new();
    let mut list = String::new();
    let mut n = 10;
    for (i, form_entries) in forms.iter().enumerate() {
        streams.push(form(100 + i as u32, form_entries, "1 0 0 rg 0 0 100 100 re f"));
        for rect in &rects {
            objs.push((n, format!("<< /Type /Annot /Subtype /Square /Rect {rect} /AP << /N {} 0 R >> >>", 100 + i)));
            list.push_str(&format!("{n} 0 R "));
            n += 1;
        }
    }
    objs.push((7, "[0 0 10 10]".to_string()));
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &streams);
    // (No answer is claimed for these but that the page is drawn and the time is short; the plain one is on the page.)
    let (b, _, _, _) = quick(&doc, 20);
    assert_eq!((b.width, b.height), (100, 100));
}

#[test]
fn appearances_and_annotation_lists_that_refer_to_themselves() {
    // A stream that draws itself, an annotation whose appearance dictionary is the annotation, an /Annots array that
    // holds itself, a state that is the annotation.
    let objs = [
        (10, "<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] /AP << /N 10 0 R >> /AS /AP >>".to_string()),
        (11, "<< /Type /Annot /Subtype /Square /Rect [20 0 30 10] /AP 11 0 R >>".to_string()),
        (12, annot(40, 0, "/AP << /N 20 0 R >>")),
        (13, "[13 0 R 12 0 R]".to_string()),
    ];
    let mut streams = squares();
    streams.push(form(20, "/BBox [0 0 10 10] /Resources << /XObject << /Me 20 0 R /Other 21 0 R >> >>", "/Me Do /Other Do 1 0 0 rg 0 0 10 10 re f"));
    streams.push(form(21, "/BBox [0 0 10 10] /Resources << /XObject << /Back 20 0 R >> >>", "/Back Do"));
    let doc = doc_of("", "<< >>", "/Annots [10 0 R 11 0 R 12 0 R 13 0 R]", "", &objs, &streams);
    let (b, w, _, _) = quick(&doc, 10);
    assert!(has(&w, "contains itself"), "{w:?}");
    assert_eq!(centre(&b, 40, 0), RED);
    // The array that is an annotation list, listing itself.
    let doc = doc_of("", "<< >>", "/Annots 13 0 R", "", &objs, &streams);
    let (b, _, _, _) = quick(&doc, 10);
    assert_eq!(centre(&b, 40, 0), RED);
}

#[test]
fn parent_chains_are_followed_a_little_and_never_in_a_circle() {
    // The value is on the third level: found. On the fortieth: not (the chain is cut), so the annotation is `Off`.
    let states = "/AP << /N << /On 5 0 R /Off 6 0 R >> >>";
    let mut objs = vec![(10, annot(0, 80, &format!("/Parent 100 0 R {states}"))), (11, annot(20, 80, &format!("/Parent 200 0 R {states}")))];
    for i in 0..60u32 {
        let (near, far) = (100 + i, 200 + i);
        let value = if i == 2 { "/V /On" } else { "" };
        objs.push((near, format!("<< /Parent {} 0 R {value} >>", near + 1)));
        let value = if i == 40 { "/V /On" } else { "" };
        objs.push((far, format!("<< /Parent {} 0 R {value} >>", far + 1)));
    }
    let doc = doc_of("", "<< >>", "/Annots [10 0 R 11 0 R]", "", &objs, &squares());
    let (b, w, _, _) = quick(&doc, 10);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 0, 80), RED);
    assert_eq!(centre(&b, 20, 80), GREEN);
}

#[test]
fn an_appearance_that_is_no_form_or_cannot_be_read_is_skipped_once() {
    // An image as the appearance, a stream whose filter is broken (shared by many annotations).
    let mut objs = vec![(10, annot(0, 0, "/AP << /N 7 0 R >>"))];
    for i in 0..500u32 {
        objs.push((20 + i, annot((i * 7) % 90, 40, "/AP << /N 8 0 R >>")));
    }
    let mut streams = squares();
    streams.push((7, "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray".to_string(), vec![0]));
    streams.push((8, "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Filter /FlateDecode".to_string(), b"not flate".to_vec()));
    let list: String = std::iter::once(10).chain(20..520).map(|i| format!("{i} 0 R ")).collect();
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &streams);
    let (b, w, _, _) = quick(&doc, 10);
    assert_eq!(centre(&b, 0, 0), WHITE);
    assert!(w.iter().any(|m| m.contains("form is skipped")), "{w:?}");
}

/// Object 7: an array of 400 thousand numbers (6.4 million bytes by `approx_size`, 45 ms to read).
fn big_array() -> (u32, String) {
    (7, format!("[{}]", "1 ".repeat(400_000)))
}

/// A page of `n` annotations built from `entries(x, y)`, with the huge array as object 7 and a small dictionary as 9.
fn crowd(n: u32, entries: impl Fn(u32, u32) -> String, extra: Vec<(u32, String)>) -> Document {
    let mut objs = vec![big_array(), (9, "<< /V /On >>".to_string())];
    objs.extend(extra);
    for i in 0..n {
        objs.push((10 + i, entries((i * 7) % 90, (i * 13) % 90)));
    }
    let list: String = (0..n).map(|i| format!("{} 0 R ", 10 + i)).collect();
    doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &objs, &squares())
}

#[test]
fn entries_that_name_a_huge_object_are_read_once() {
    // R3c2c-1: `/Subtype 7 0 R /F 7 0 R` with 7 an array of 400 thousand numbers: each annotation read it twice (over 100 s
    // for 20 thousand). The first read is charged by the size, the rest are remembered and cost a little.
    let n = 4000;
    let (b, w, work, _) = quick(&crowd(n, |x, y| format!("<< /Subtype 7 0 R /F 7 0 R /Rect [{x} {y} {} {}] {AP_RED} >>", x + 10, y + 10), vec![]), 5);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 0, 0), RED);
    // The same with entries that are small: the difference is one reading of the array.
    let (_, _, plain, _) = quick(&crowd(n, |x, y| format!("<< /Subtype /Square /F 0 /Rect [{x} {y} {} {}] {AP_RED} >>", x + 10, y + 10), vec![]), 5);
    let parse = 400_000.0 * 16.0 * cost::RESOLVE_BYTE;
    assert!(work - plain >= parse * 0.9 && work - plain < parse * 1.2, "{work} {plain}");
}

#[test]
fn a_parent_that_is_a_huge_array_is_read_once() {
    // R3c2c-2: no `/AS`, so the `/Parent` is looked at, and it is the same huge array in every widget.
    let n = 6000;
    let widget = |parent: u32| move |x: u32, y: u32| format!("<< /Subtype /Widget /Parent {parent} 0 R /Rect [{x} {y} {} {}] /AP << /N 8 0 R >> >>", x + 10, y + 10);
    let states = vec![(8, "<< /On 5 0 R /Off 6 0 R >>".to_string())];
    let (b, w, work, _) = quick(&crowd(n, widget(7), states.clone()), 5);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 0, 0), GREEN);
    // A parent that is a small dictionary without a value costs the same but for the one reading of the array.
    let (_, _, plain, _) = quick(&crowd(n, widget(9), states), 5);
    let parse = 400_000.0 * 16.0 * cost::RESOLVE_BYTE;
    assert!(work - plain >= parse * 0.9 && work - plain < parse * 1.2, "{work} {plain}");
}

#[test]
fn resources_that_many_appearance_streams_share_are_made_once() {
    // The appearance streams are all different and name one resource dictionary (object 8) that has 50 thousand fonts: it
    // was read and copied for each of them.
    let n = 2000u32;
    let fonts: String = (0..50_000).map(|i| format!("/F{i} 5 0 R ")).collect();
    let objs = vec![(8, format!("<< /Font << {fonts} >> >>"))];
    let mut streams = vec![];
    let mut annots = vec![];
    for i in 0..n {
        streams.push(form(1000 + i, "/BBox [0 0 10 10] /Resources 8 0 R", &format!("1 0 0 rg 0 0 10 10 re f % {i}")));
        annots.push((10 + i, annot((i * 7) % 90, (i * 13) % 90, &format!("/AP << /N {} 0 R >>", 1000 + i))));
    }
    let list: String = (0..n).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let all: Vec<(u32, String)> = objs.into_iter().chain(annots).collect();
    let doc = doc_of("", "<< >>", &format!("/Annots [{list}]"), "", &all, &streams);
    let (b, w, work, _) = quick(&doc, 5);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 0, 0), RED);
    // One reading of the dictionary (about 1.3 million bytes, 11 ns a byte), and the visits and drawings.
    assert!(work < 30e6 + f64::from(n) * 40_000.0, "{work}");
}

#[test]
fn numbers_by_reference_in_a_rectangle_and_in_a_box_are_read() {
    // The numbers of `/Rect` and of `/BBox` are indirect objects (7.3.10 allows it anywhere).
    let objs = vec![(20, "50".to_string()), (21, "60".to_string()), (22, "10".to_string()), (11, "<< /Type /Annot /Subtype /Square /Rect [20 0 R 20 0 R 21 0 R 21 0 R] /AP << /N 9 0 R >> >>".to_string())];
    let streams = vec![form(9, "/BBox [0 0 22 0 R 22 0 R]", "1 0 0 rg 0 0 10 10 re f")];
    let doc = doc_of("", "<< >>", "/Annots [11 0 R]", "", &objs, &streams);
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(centre(&b, 50, 50), RED);
}

#[test]
fn a_box_that_is_flat_is_not_scaled_in_that_direction() {
    // A horizontal line's appearance has a box of no height (`[0 0 100 0]`): PDFium puts it on the rectangle with the
    // scale 1 for that direction, and the line is drawn along the bottom edge of the rectangle.
    let line = "<< /Type /Annot /Subtype /Line /Rect [0 40 100 60] /AP << /N 9 0 R >> >>".to_string();
    let streams = vec![form(9, "/BBox [0 0 100 0]", "0 0 1 RG 6 w 0 0 m 100 0 l S")];
    let doc = doc_of("", "<< >>", "/Annots [11 0 R]", "", &[(11, line)], &streams);
    let (b, w) = render(&doc, true);
    assert!(w.is_empty(), "{w:?}");
    // The rectangle's bottom edge is the row 60; the line, a pixel high there, and not along the rectangle's height.
    assert_eq!(pixel(&b, 50, 60), BLUE);
    let drawn = (0..100u32).filter(|y| pixel(&b, 50, *y) == BLUE).count();
    assert_eq!(drawn, 1);
}
