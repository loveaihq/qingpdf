//! Shapes and changes to annotations that are on the page (2026-10-17), worked out against small files made by hand.

use super::modify::numbers;
use super::*;
use crate::testutil::PdfBuilder;

const NOW: &str = "D:20261017120000Z";

/// A one-page file (200 x 300 points unless `page` says otherwise) with the annotation objects `annots` (numbered from 10) in its list.
fn file(page: &str, annots: &[&str], extra: impl FnOnce(&mut PdfBuilder)) -> Document {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    let refs: String = (0..annots.len()).map(|i| format!("{} 0 R ", 10 + i)).collect();
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources << >> {page} /Annots [{refs}] >>"));
    for (i, a) in annots.iter().enumerate() {
        b.obj(10 + i as u32, a);
    }
    extra(&mut b);
    Document::from_bytes(b.finish_classic(40, "/Root 1 0 R")).unwrap()
}

fn run(doc: &Document, edit: &Edit) -> Result<()> {
    let pages = doc.pages().unwrap();
    let (objects, _) = apply_any(doc, &pages, edit, NOW, &Work::new(), DEFAULT_ROOM)?;
    commit(doc, objects);
    Ok(())
}

fn dict(doc: &Document, num: u32) -> Dict {
    doc.get(ObjRef::new(num, 0)).unwrap().as_dict().unwrap().clone()
}

fn nums(doc: &Document, d: &Dict, key: &str) -> Vec<f64> {
    numbers(doc, d.get(key), 100_000).unwrap_or_else(|| panic!("no {key}"))
}

fn near(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len(), "{a:?} / {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-6, "{a:?} is not {b:?}");
    }
}

fn last_annot(doc: &Document) -> (u32, Dict) {
    let pages = doc.pages().unwrap();
    let page = doc.get(pages[0].obj_ref).unwrap();
    let list = doc.resolve(page.as_dict().unwrap().get("Annots").unwrap()).unwrap();
    let r = list.as_array().unwrap().last().unwrap().as_obj_ref().unwrap();
    (r.num, dict(doc, r.num))
}

fn stream_text(doc: &Document, d: &Dict) -> String {
    let ap = doc.resolve(d.get("AP").unwrap()).unwrap();
    let n = ap.as_dict().unwrap().get("N").unwrap().as_obj_ref().unwrap();
    let Object::Stream(s) = doc.get(n).unwrap() else { panic!("no stream") };
    String::from_utf8_lossy(&doc.decode_stream(&s).unwrap()).into_owned()
}

fn shape(kind: ShapeKind, from: [f32; 2], to: [f32; 2], fill: bool) -> Edit {
    Edit::Shape { page: 0, kind, from, to, color: [230, 30, 30], width: 4.0, fill, author: "me".to_string() }
}

#[test]
fn each_shape_is_where_it_was_dragged_with_its_pen_inside_its_rectangle() {
    let doc = file("", &[], |_| {});
    // Dragged from the lower right to the upper left: the same rectangle. The page is 300 high, so shown y 30..90 is user y 210..270.
    run(&doc, &shape(ShapeKind::Rectangle, [120.0, 90.0], [20.0, 30.0], false)).unwrap();
    let (_, d) = last_annot(&doc);
    assert_eq!(d.get_name("Subtype").unwrap().as_bytes(), b"Square");
    near(&nums(&doc, &d, "Rect"), &[18.0, 208.0, 122.0, 272.0]);
    near(&nums(&doc, &d, "RD"), &[2.0, 2.0, 2.0, 2.0]);
    near(&nums(&doc, &d, "C"), &[230.0 / 255.0, 30.0 / 255.0, 30.0 / 255.0]);
    assert!(d.get("IC").is_none() && d.get_int("F") == Some(4));
    let bs = doc.resolve(d.get("BS").unwrap()).unwrap();
    assert_eq!(bs.as_dict().unwrap().get("W").and_then(Object::as_f64), Some(4.0));
    assert!(stream_text(&doc, &d).contains("20 210 100 60 re\nS"), "{}", stream_text(&doc, &d));

    run(&doc, &shape(ShapeKind::Ellipse, [20.0, 30.0], [120.0, 90.0], true)).unwrap();
    let (_, d) = last_annot(&doc);
    assert_eq!(d.get_name("Subtype").unwrap().as_bytes(), b"Circle");
    let fill = nums(&doc, &d, "IC");
    assert!(fill[0] > 0.9 && fill[1] > 0.7 && fill[1] == fill[2], "{fill:?}");
    let text = stream_text(&doc, &d);
    assert!(text.starts_with("q\n/GS0 gs\n") && text.contains(" c\nh\n") && text.trim_end().ends_with('S'), "{text}");
    let ap = doc.resolve(d.get("AP").unwrap()).unwrap();
    let Object::Stream(form) = doc.get(ap.as_dict().unwrap().get("N").unwrap().as_obj_ref().unwrap()).unwrap() else { panic!("no form") };
    assert!(form.dict.get("Resources").is_some_and(|r| r.as_dict().is_some_and(|r| r.contains_key("ExtGState"))));
    near(&nums(&doc, &form.dict, "BBox"), &nums(&doc, &d, "Rect"));

    run(&doc, &shape(ShapeKind::Arrow, [20.0, 150.0], [120.0, 150.0], false)).unwrap();
    let (_, d) = last_annot(&doc);
    assert_eq!(d.get_name("Subtype").unwrap().as_bytes(), b"Line");
    near(&nums(&doc, &d, "L"), &[20.0, 150.0, 120.0, 150.0]);
    let le = doc.resolve(d.get("LE").unwrap()).unwrap();
    let names: Vec<&[u8]> = le.as_array().unwrap().iter().map(|n| n.as_name().unwrap().as_bytes()).collect();
    assert_eq!(names, [b"None".as_slice(), b"OpenArrow".as_slice()]);
    assert_eq!(d.get_name("IT").unwrap().as_bytes(), b"LineArrow");
    let rect = nums(&doc, &d, "Rect");
    assert!(rect[0] < 20.0 && rect[2] > 120.0 && rect[1] < 150.0 && rect[3] > 150.0, "{rect:?}");

    run(&doc, &shape(ShapeKind::Line, [20.0, 20.0], [60.0, 60.0], true)).unwrap();
    let (_, d) = last_annot(&doc);
    let le = doc.resolve(d.get("LE").unwrap()).unwrap();
    assert!(le.as_array().unwrap().iter().all(|n| n.as_name().unwrap().as_bytes() == b"None"));
    assert!(d.get("IC").is_none() && d.get("IT").is_none(), "a fill and an intent are for the other kinds");
    assert_eq!(doc.resolve(d.get("Contents").unwrap()).unwrap(), Object::String(PdfString::literal(Vec::new())));
}

#[test]
fn shapes_that_are_too_small_or_not_places_are_refused_and_the_page_is_not_changed() {
    let doc = file("", &[], |_| {});
    let before = doc.overlay_bytes();
    assert!(run(&doc, &shape(ShapeKind::Rectangle, [20.0, 30.0], [21.0, 90.0], false)).is_err());
    assert!(run(&doc, &shape(ShapeKind::Line, [20.0, 30.0], [20.5, 30.5], false)).is_err());
    assert!(run(&doc, &shape(ShapeKind::Ellipse, [f32::NAN, 30.0], [50.0, 90.0], false)).is_err());
    assert_eq!(doc.overlay_bytes(), before);
    // Off the page it is held to the page; a width that is not a number is one point.
    let mut edit = shape(ShapeKind::Rectangle, [-50.0, -50.0], [900.0, 900.0], false);
    if let Edit::Shape { width, .. } = &mut edit {
        *width = f32::NAN;
    }
    run(&doc, &edit).unwrap();
    let (_, d) = last_annot(&doc);
    near(&nums(&doc, &d, "Rect"), &[-0.5, -0.5, 200.5, 300.5]);
}

#[test]
fn shapes_on_a_turned_page_with_a_crop_box_are_where_the_page_shows_them() {
    for rotate in [0, 90, 180, 270] {
        let doc = file(&format!("/CropBox [50 50 250 250] /MediaBox [0 0 300 300] /Rotate {rotate}"), &[], |_| {});
        run(&doc, &shape(ShapeKind::Rectangle, [10.0, 20.0], [60.0, 40.0], false)).unwrap();
        let (_, d) = last_annot(&doc);
        let rect = nums(&doc, &d, "Rect");
        let shown = Shown::of(&doc.pages().unwrap()[0]);
        let b = modify::shown_rect(&shown, 0, [rect[0], rect[1], rect[2], rect[3]]);
        // The shape's rectangle grown by half the pen, as the person sees it.
        near(&b, &[8.0, 18.0, 62.0, 42.0]);
    }
}

fn markup() -> String {
    "<< /Type /Annot /Subtype /Highlight /Rect [10 20 110 40] /C [1 1 0] /F 4 /QuadPoints [10 40 110 40 10 20 110 20] /Popup 11 0 R >>".to_string()
}

#[test]
fn moving_changes_the_rectangle_and_every_point_by_the_same_distance_and_keeps_the_appearance() {
    let ink = "<< /Type /Annot /Subtype /Ink /Rect [10 100 60 150] /InkList [[10 100 30 120] [40 140 60 150]] /AP << /N 20 0 R >> >>";
    let line = "<< /Type /Annot /Subtype /Line /Rect [70 100 120 150] /L [70 100 120 150] /AP << /N 20 0 R >> >>";
    let hl = markup();
    let popup = "<< /Type /Annot /Subtype /Popup /Rect [120 20 180 60] /Parent 10 0 R >>";
    for rotate in [0, 90, 180, 270] {
        let doc = file(&format!("/Rotate {rotate}"), &[&hl, popup, ink, line], |b| {
            b.stream_obj(20, "/Type /XObject /Subtype /Form /BBox [0 0 50 50]", b"0 0 m 50 50 l S");
        });
        let shown = Shown::of(&doc.pages().unwrap()[0]);
        let at = |doc: &Document, num: u32| modify::shown_rect(&shown, 0, nums(doc, &dict(doc, num), "Rect").as_slice().try_into().unwrap());
        let before: Vec<[f64; 4]> = [10, 12, 13].iter().map(|&n| at(&doc, n)).collect();
        // 12 points to the right and 7 down, as the page is shown.
        for index in [0u32, 2, 3] {
            let num = 10 + index;
            run(&doc, &Edit::Move { page: 0, index, num, delta: [12.0, 7.0] }).unwrap();
        }
        let after: Vec<[f64; 4]> = [10, 12, 13].iter().map(|&n| at(&doc, n)).collect();
        for (b, a) in before.iter().zip(&after) {
            near(a, &[b[0] + 12.0, b[1] + 7.0, b[2] + 12.0, b[3] + 7.0]);
        }
        // The points moved with the rectangle: the first corner of the line is on the moved rectangle's corner still.
        let l = nums(&doc, &dict(&doc, 13), "L");
        let (p, q) = (shown.map(l[0], l[1]), shown.map(l[2], l[3]));
        let r = after[2];
        near(&[p.0.min(q.0), p.1.min(q.1), p.0.max(q.0), p.1.max(q.1)], &r);
        let strokes = doc.resolve(dict(&doc, 12).get("InkList").unwrap()).unwrap();
        assert_eq!(strokes.as_array().unwrap().len(), 2);
        let first = numbers(&doc, strokes.as_array().unwrap().first(), 100).unwrap();
        let (m, o) = (shown.map(first[0], first[1]), shown.map(10.0, 100.0));
        near(&[m.0, m.1], &[o.0 + 12.0, o.1 + 7.0]);
        // The highlight's popup went with it; the appearance is the file's own.
        let popup_rect = at(&doc, 11);
        let popup_before = modify::shown_rect(&shown, 0, [120.0, 20.0, 180.0, 60.0]);
        near(&popup_rect, &[popup_before[0] + 12.0, popup_before[1] + 7.0, popup_before[2] + 12.0, popup_before[3] + 7.0]);
        assert_eq!(dict(&doc, 12).get("AP"), doc.get(ObjRef::new(12, 0)).unwrap().as_dict().unwrap().get("AP"));
        assert!(dict(&doc, 12).get("M").is_some());
    }
}

#[test]
fn a_note_moves_by_its_corner_and_a_move_keeps_some_of_the_annotation_on_the_page() {
    let note = "<< /Type /Annot /Subtype /Text /Rect [20 260 40 280] /F 28 /Contents (hello) >>";
    let doc = file("", &[note], |_| {});
    run(&doc, &Edit::Move { page: 0, index: 0, num: 10, delta: [30.0, 10.0] }).unwrap();
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[50.0, 250.0, 70.0, 270.0]);
    // A shove far off the page stops with 8 points of it still on.
    run(&doc, &Edit::Move { page: 0, index: 0, num: 10, delta: [-5000.0, 5000.0] }).unwrap();
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[-12.0, -12.0, 8.0, 8.0]);
}

#[test]
fn what_cannot_be_changed_is_refused_with_a_reason_and_leaves_the_file_alone() {
    let locked = "<< /Type /Annot /Subtype /Square /Rect [10 10 60 60] /F 132 /C [1 0 0] >>";
    let text_locked = "<< /Type /Annot /Subtype /Text /Rect [70 10 90 30] /F 516 /Contents (a) >>";
    let link = "<< /Type /Annot /Subtype /Link /Rect [100 10 150 60] >>";
    let read_only = "<< /Type /Annot /Subtype /Square /Rect [10 70 60 120] /F 68 /C [1 0 0] >>";
    let doc = file("", &[locked, text_locked, link, read_only], |_| {});
    let why = |e: Edit| run(&doc, &e).unwrap_err().to_string();
    assert!(why(Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 5.0] }).contains("locked"));
    assert!(why(Edit::Resize { page: 0, index: 0, num: 10, rect: [10.0, 10.0, 90.0, 90.0] }).contains("locked"));
    assert!(why(Edit::Restyle { page: 0, index: 0, num: 10, color: Some([0, 0, 255]), width: None, fill: None }).contains("locked"));
    // Locked does not restrict the text (Table 165); read-only does.
    run(&doc, &Edit::SetText { page: 0, index: 0, num: 10, text: "x".to_string() }).unwrap();
    assert!(why(Edit::SetText { page: 0, index: 3, num: 13, text: "x".to_string() }).contains("locked"));
    assert!(why(Edit::Move { page: 0, index: 3, num: 13, delta: [5.0, 5.0] }).contains("locked"));
    // Contents locked: the text stays, but it can be moved and recoloured.
    assert!(why(Edit::SetText { page: 0, index: 1, num: 11, text: "x".to_string() }).contains("locked"));
    run(&doc, &Edit::Move { page: 0, index: 1, num: 11, delta: [5.0, 5.0] }).unwrap();
    run(&doc, &Edit::Restyle { page: 0, index: 1, num: 11, color: Some([0, 0, 255]), width: None, fill: None }).unwrap();
    // A link is part of the page; a list that has changed (the wrong number) or a place that is not there is refused too.
    assert!(why(Edit::Move { page: 0, index: 2, num: 12, delta: [5.0, 5.0] }).contains("part of the page"));
    assert!(why(Edit::Move { page: 0, index: 1, num: 99, delta: [5.0, 5.0] }).contains("not there"));
    assert!(why(Edit::Move { page: 0, index: 9, num: 11, delta: [5.0, 5.0] }).contains("not there"));
    assert!(why(Edit::Move { page: 0, index: 1, num: 11, delta: [f32::INFINITY, 5.0] }).contains("not a number"));
}

#[test]
fn a_text_markup_is_drawn_again_in_the_new_colour_whatever_order_its_corners_are_in() {
    // The corners counterclockwise from the lower left, as the specification says (Acrobat uses another order).
    let hl = "<< /Type /Annot /Subtype /Highlight /Rect [10 20 110 40] /C [1 1 0] /QuadPoints [10 20 110 20 110 40 10 40] >>";
    let doc = file("", &[hl], |_| {});
    run(&doc, &Edit::Restyle { page: 0, index: 0, num: 10, color: Some([0, 0, 255]), width: None, fill: None }).unwrap();
    let d = dict(&doc, 10);
    near(&nums(&doc, &d, "C"), &[0.0, 0.0, 1.0]);
    // One rectangle filled with blue, from the upper left round to the lower left.
    let text = stream_text(&doc, &d);
    assert!(text.contains("0 0 1 rg") && text.contains("10 40 m\n110 40 l\n110 20 l\n10 20 l\nh f"), "{text}");
    near(&nums(&doc, &d, "Rect"), &[10.0, 20.0, 110.0, 40.0]);
    // The width of a highlight is not a thing: it is left as it is.
    run(&doc, &Edit::Restyle { page: 0, index: 0, num: 10, color: None, width: Some(9.0), fill: None }).unwrap();
}

#[test]
fn a_square_is_recoloured_filled_and_given_a_wider_line_inside_the_same_rectangle() {
    let sq = "<< /Type /Annot /Subtype /Square /Rect [10 10 110 60] /C [1 0 0] /BS << /W 2 >> /IC [1 1 0] >>";
    let doc = file("", &[sq], |_| {});
    run(&doc, &Edit::Restyle { page: 0, index: 0, num: 10, color: Some([0, 0, 255]), width: Some(6.0), fill: Some(false) }).unwrap();
    let d = dict(&doc, 10);
    // (The shape stays where it was, on the middle of its line: the rectangle grows with the pen.)
    near(&nums(&doc, &d, "Rect"), &[8.0, 8.0, 112.0, 62.0]);
    near(&nums(&doc, &d, "RD"), &[3.0, 3.0, 3.0, 3.0]);
    assert!(d.get("IC").is_none());
    assert!(stream_text(&doc, &d).contains("11 11 98 48 re\nS"), "{}", stream_text(&doc, &d));
    run(&doc, &Edit::Restyle { page: 0, index: 0, num: 10, color: None, width: None, fill: Some(true) }).unwrap();
    assert!(dict(&doc, 10).get("IC").is_some());
    // A shape that its /RD leaves nothing of is refused; a thick line only makes the rectangle grow.
    let gone = file("", &["<< /Type /Annot /Subtype /Circle /Rect [10 10 40 30] /RD [20 20 20 20] >>"], |_| {});
    assert!(run(&gone, &Edit::Restyle { page: 0, index: 0, num: 10, color: None, width: Some(4.0), fill: None }).is_err());
    let small = file("", &["<< /Type /Annot /Subtype /Circle /Rect [10 10 40 30] >>"], |_| {});
    run(&small, &Edit::Restyle { page: 0, index: 0, num: 10, color: None, width: Some(20.0), fill: None }).unwrap();
    near(&nums(&small, &dict(&small, 10), "Rect"), &[0.5, 0.5, 49.5, 39.5]);
    // A kind the reader does not draw is refused.
    let stamp = "<< /Type /Annot /Subtype /Stamp /Rect [10 100 60 150] >>";
    let doc = file("", &[stamp], |_| {});
    assert!(run(&doc, &Edit::Restyle { page: 0, index: 0, num: 10, color: Some([1, 2, 3]), width: None, fill: None }).is_err());
}

#[test]
fn a_square_made_by_another_program_stays_where_its_rd_puts_it() {
    // The shape is `/Rect` less `/RD` (here 5 points all round), whatever the pen is.
    let sq = "<< /Type /Annot /Subtype /Square /Rect [10 10 110 60] /RD [5 5 5 5] /C [1 0 0] /BS << /W 2 >> >>";
    let doc = file("", &[sq], |_| {});
    run(&doc, &Edit::Restyle { page: 0, index: 0, num: 10, color: Some([0, 0, 255]), width: None, fill: None }).unwrap();
    let d = dict(&doc, 10);
    near(&nums(&doc, &d, "Rect"), &[14.0, 14.0, 106.0, 56.0]);
    near(&nums(&doc, &d, "RD"), &[1.0, 1.0, 1.0, 1.0]);
    assert!(stream_text(&doc, &d).contains("15 15 90 40 re\nS"), "{}", stream_text(&doc, &d));
}

#[test]
fn what_would_lose_something_when_drawn_again_is_not_drawn_again() {
    let cloud = "<< /Type /Annot /Subtype /Square /Rect [10 10 110 60] /BE << /S /C /I 1 >> >>";
    let leader = "<< /Type /Annot /Subtype /Line /Rect [10 100 110 150] /L [10 100 110 150] /LL 20 /C [0 0 1] >>";
    let caption = "<< /Type /Annot /Subtype /Line /Rect [10 160 110 200] /L [10 160 110 200] /Cap true /C [0 0 1] >>";
    let circle_end = "<< /Type /Annot /Subtype /Line /Rect [10 210 110 250] /L [10 210 110 250] /LE [/Circle /None] /C [0 0 1] >>";
    let callout = "<< /Type /Annot /Subtype /FreeText /Rect [120 10 190 60] /DA (0 0 0 rg /Helv 12 Tf) /Contents (hi) /CL [120 60 100 80] >>";
    let doc = file("", &[cloud, leader, caption, circle_end, callout], |_| {});
    for (i, num) in (0..5).zip(10..) {
        let r = run(&doc, &Edit::Restyle { page: 0, index: i, num, color: Some([0, 0, 255]), width: None, fill: None });
        assert!(r.is_err(), "annotation {i} was drawn again");
        let r = run(&doc, &Edit::Resize { page: 0, index: i, num, rect: [20.0, 20.0, 150.0, 150.0] });
        assert!(r.is_err(), "annotation {i} was drawn again in a new place");
    }
    // A moved one keeps what it has (the appearance is the one it had).
    run(&doc, &Edit::Move { page: 0, index: 1, num: 11, delta: [5.0, 5.0] }).unwrap();
    // The size of a text box's text is held to the sizes the reader draws (a size of 100000 in /DA does not make a huge appearance).
    let big = "<< /Type /Annot /Subtype /FreeText /Rect [10 10 190 60] /DA (0 0 0 rg /Helv 100000 Tf) /Border [0 0 0] /Contents (hi) >>";
    let doc = file("", &[big], |_| {});
    run(&doc, &Edit::SetText { page: 0, index: 0, num: 10, text: "hello".to_string() }).unwrap();
    let text = stream_text(&doc, &dict(&doc, 10));
    assert!(!text.contains("100000"), "{text}");
}

#[test]
fn resizing_draws_a_shape_again_and_stretches_what_the_reader_cannot_draw() {
    let sq = "<< /Type /Annot /Subtype /Square /Rect [10 10 110 60] /C [1 0 0] /BS << /W 2 >> >>";
    let stamp = "<< /Type /Annot /Subtype /Stamp /Rect [10 100 60 150] /AP << /N 20 0 R >> >>";
    let line = "<< /Type /Annot /Subtype /Line /Rect [70 100 120 150] /L [70 100 120 150] /LE [/None /OpenArrow] /C [0 0 1] >>";
    let note = "<< /Type /Annot /Subtype /Text /Rect [20 260 40 280] /F 28 >>";
    let doc = file("", &[sq, stamp, line, note], |b| {
        b.stream_obj(20, "/Type /XObject /Subtype /Form /BBox [0 0 50 50]", b"0 0 m 50 50 l S");
    });
    // The square: new rectangle (shown 20,100 to 120,200), drawn again with its own pen.
    run(&doc, &Edit::Resize { page: 0, index: 0, num: 10, rect: [20.0, 100.0, 120.0, 200.0] }).unwrap();
    let d = dict(&doc, 10);
    near(&nums(&doc, &d, "Rect"), &[20.0, 100.0, 120.0, 200.0]);
    assert!(stream_text(&doc, &d).contains("21 101 98 98 re\nS"), "{}", stream_text(&doc, &d));
    // The picture: only the rectangle, and its appearance is the one it had.
    let before = dict(&doc, 11).get("AP").cloned();
    run(&doc, &Edit::Resize { page: 0, index: 1, num: 11, rect: [10.0, 100.0, 90.0, 180.0] }).unwrap();
    let d = dict(&doc, 11);
    near(&nums(&doc, &d, "Rect"), &[10.0, 120.0, 90.0, 200.0]);
    assert_eq!(d.get("AP").cloned(), before);
    // The line: its ends are carried into the new rectangle in proportion, with a new appearance.
    run(&doc, &Edit::Resize { page: 0, index: 2, num: 12, rect: [60.0, 100.0, 160.0, 200.0] }).unwrap();
    let d = dict(&doc, 12);
    let l = nums(&doc, &d, "L");
    // (The rectangle was [70 100 120 150], shown y 150..200: now shown 100..200 and x 60..160.)
    near(&l, &[60.0, 100.0, 160.0, 200.0]);
    assert!(stream_text(&doc, &d).contains("S"));
    // The icon of a note keeps its size.
    assert!(run(&doc, &Edit::Resize { page: 0, index: 3, num: 13, rect: [20.0, 20.0, 80.0, 80.0] }).is_err());
}

#[test]
fn the_text_of_a_note_is_set_and_a_stale_rich_text_goes() {
    let note = "<< /Type /Annot /Subtype /Text /Rect [20 260 40 280] /F 28 /Contents (old) /RC (<body>old</body>) >>";
    let doc = file("", &[note], |_| {});
    run(&doc, &Edit::SetText { page: 0, index: 0, num: 10, text: "new \u{4E2D}\u{6587}".to_string() }).unwrap();
    let d = dict(&doc, 10);
    assert!(d.get("RC").is_none());
    let Object::String(s) = doc.resolve(d.get("Contents").unwrap()).unwrap() else { panic!("no text") };
    assert_eq!(crate::info::decode_text_string(&s.bytes), "new \u{4E2D}\u{6587}");
    assert_eq!(modify::contents_of(&doc, &d, 100), "new \u{4E2D}\u{6587}");
}

fn form_dict(doc: &Document, d: &Dict) -> Dict {
    let ap = doc.resolve(d.get("AP").unwrap()).unwrap();
    let n = ap.as_dict().unwrap().get("N").unwrap().as_obj_ref().unwrap();
    let Object::Stream(s) = doc.get(n).unwrap() else { panic!("no stream") };
    s.dict
}

fn restyle_blue(doc: &Document, index: u32, num: u32) -> Result<()> {
    run(doc, &Edit::Restyle { page: 0, index, num, color: Some([0, 0, 255]), width: None, fill: None })
}

#[test]
fn the_opacity_of_an_annotation_is_kept_when_it_is_drawn_again() {
    // A highlighter pen at 30 %: recoloured, it is still at 30 %, and a highlight still multiplies.
    let hl = "<< /Type /Annot /Subtype /Highlight /Rect [10 20 110 40] /C [1 1 0] /CA 0.3 /QuadPoints [10 20 110 20 110 40 10 40] >>";
    let ink = "<< /Type /Annot /Subtype /Ink /Rect [10 70 110 120] /InkList [[10 70 110 120]] /C [1 0 0] /CA 0.5 >>";
    let doc = file("", &[hl, ink], |_| {});
    restyle_blue(&doc, 0, 10).unwrap();
    restyle_blue(&doc, 1, 11).unwrap();
    for (num, name) in [(10, "highlight"), (11, "drawing")] {
        let d = dict(&doc, num);
        assert!(d.get("CA").is_some(), "{name}: the key stays");
        assert!(stream_text(&doc, &d).starts_with("/GS1 gs\n"), "{name}: {}", stream_text(&doc, &d));
        let form = form_dict(&doc, &d);
        let Some(Object::Dict(res)) = form.get("Resources") else { panic!("no resources") };
        let Some(Object::Dict(states)) = res.get("ExtGState") else { panic!("no states") };
        let Some(Object::Dict(gs1)) = states.get("GS1") else { panic!("no GS1") };
        let want = if num == 10 { 0.3 } else { 0.5 };
        assert_eq!((gs1.get("CA").and_then(Object::as_f64), gs1.get("ca").and_then(Object::as_f64)), (Some(want), Some(want)), "{name}");
        assert_eq!(states.get("GS0").is_some(), num == 10, "{name}: only the highlight multiplies");
    }
    // Without /CA nothing is added.
    let plain = file("", &["<< /Type /Annot /Subtype /Ink /Rect [10 70 110 120] /InkList [[10 70 110 120]] /C [1 0 0] >>"], |_| {});
    restyle_blue(&plain, 0, 10).unwrap();
    assert!(!stream_text(&plain, &dict(&plain, 10)).contains("GS1"));
}

#[test]
fn a_line_keeps_its_interior_colour_and_a_note_its_name() {
    let line = "<< /Type /Annot /Subtype /Line /Rect [10 100 110 150] /L [10 100 110 150] /LE [/None /ClosedArrow] /C [1 0 0] /IC [0 1 0] >>";
    let key = "<< /Type /Annot /Subtype /Text /Rect [20 260 40 280] /F 28 /Name /Key >>";
    let comment = "<< /Type /Annot /Subtype /Text /Rect [50 260 70 280] /F 28 /Name /Comment >>";
    let unnamed = "<< /Type /Annot /Subtype /Text /Rect [80 260 100 280] /F 28 >>";
    let doc = file("", &[line, key, comment, unnamed], |_| {});
    restyle_blue(&doc, 0, 10).unwrap();
    let d = dict(&doc, 10);
    near(&nums(&doc, &d, "IC"), &[0.0, 1.0, 0.0]);
    let text = stream_text(&doc, &d);
    // The head is filled with the interior colour, the line is blue.
    assert!(text.contains("0 1 0 rg") && text.contains("0 0 1 RG") && text.contains("b\n"), "{text}");
    // The icon is the speech bubble: a note called something else is not drawn as one.
    assert!(restyle_blue(&doc, 1, 11).is_err());
    restyle_blue(&doc, 2, 12).unwrap();
    assert_eq!(dict(&doc, 12).get_name("Name").map(|n| n.as_bytes().to_vec()), Some(b"Comment".to_vec()));
    restyle_blue(&doc, 3, 13).unwrap();
    assert_eq!(dict(&doc, 13).get_name("Name").map(|n| n.as_bytes().to_vec()), Some(b"Comment".to_vec()));
    // The other note can still be moved and given a text.
    run(&doc, &Edit::Move { page: 0, index: 1, num: 11, delta: [5.0, 5.0] }).unwrap();
    run(&doc, &Edit::SetText { page: 0, index: 1, num: 11, text: "kept".to_string() }).unwrap();
}

#[test]
fn what_would_be_lost_is_not_drawn_again_but_can_still_be_moved() {
    let square = |extra: &str| format!("<< /Type /Annot /Subtype /Square /Rect [10 10 110 60] /C [1 0 0] {extra} >>");
    let ink = |extra: &str| format!("<< /Type /Annot /Subtype /Ink /Rect [10 70 110 120] /InkList [[10 70 110 120]] /C [1 0 0] {extra} >>");
    let boxed = |extra: &str| format!("<< /Type /Annot /Subtype /FreeText /Rect [10 130 190 180] /DA (0 0 0 rg /Helv 12 Tf) /Contents (hi) {extra} >>");
    let long = format!("<< /Type /Annot /Subtype /FreeText /Rect [10 130 190 180] /DA (0 0 0 rg /Helv 12 Tf) /Border [0 0 0] /Contents ({}) >>", "x".repeat(4001));
    // (the annotation, whether it is drawn again)
    let cases = [
        (square("/BS << /W 2 >>"), true),
        (square("/Border [0 0 2 []]"), true),
        (square("/BS << /W 2 /S /D /D [3 2] >>"), false),
        (square("/BS << /W 2 /D [3 2] >>"), false),
        (square("/BS << /W 2 /S /D >>"), false),
        (square("/Border [0 0 2 [3 2]]"), false),
        (square("/BS << /W 0 >>"), false),
        (square("/Border [0 0 0]"), false),
        (square("/BS << /W 2 >> /BE << /S /C /I 1 >>"), false),
        (ink(""), true),
        (ink("/BS << /W 2 /S /D /D [3 2] >>"), false),
        (ink("/Path [[10 70 110 120]]"), false),
        (boxed("/Border [0 0 0] /Q 0"), true),
        (boxed("/Border [0 0 0] /CA 1"), true),
        (boxed("/Border [0 0 0] /RC (<body>hi</body>)"), false),
        (boxed("/Border [0 0 0] /DS (font: 12pt)"), false),
        (boxed("/Border [0 0 0] /Q 1"), false),
        (boxed("/Border [0 0 0] /C [1 1 0]"), false),
        (boxed("/Border [0 0 0] /IC [1 1 0]"), false),
        (boxed("/Border [0 0 0] /BE << /S /C >>"), false),
        (boxed("/Border [0 0 0] /CL [10 130 5 120]"), false),
        (boxed("/Border [0 0 0] /CA 0.5"), false),
        (boxed("/BS << /W 1 >>"), false),
        (boxed(""), false),
        (long, false),
    ];
    for (i, (annot, again)) in cases.iter().enumerate() {
        let doc = file("", &[annot], |_| {});
        let r = restyle_blue(&doc, 0, 10);
        assert_eq!(r.is_ok(), *again, "case {i}: {annot}: {r:?}");
        if let Err(e) = &r
            && !*again
        {
            assert!(e.to_string().contains("cannot draw again") || e.to_string().contains("too long"), "case {i}: {e}");
        }
        // What cannot be drawn again can still be moved (and the text of a note or a short text box changed).
        run(&doc, &Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 5.0] }).unwrap();
    }
}

#[test]
fn a_text_box_keeps_its_rectangle_when_recoloured_and_a_locked_or_overhanging_one_is_not_moved_by_a_text_edit() {
    let own = |rect: &str, flags: &str| format!("<< /Type /Annot /Subtype /FreeText /Rect {rect} {flags} /DA (0 0 0 rg /Helv 12 Tf) /Contents (hi) /Border [0 0 0] >>");
    let set = |text: &str| Edit::SetText { page: 0, index: 0, num: 10, text: text.to_string() };
    // Hangs over the right edge of the 200 wide page: recolouring leaves the rectangle where it is, a text edit does not move it (it is refused).
    let doc = file("", &[&own("[150 200 260 250]", "")], |_| {});
    restyle_blue(&doc, 0, 10).unwrap();
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[150.0, 200.0, 260.0, 250.0]);
    let why = run(&doc, &set("hello")).unwrap_err().to_string();
    assert!(why.contains("edge of the page"), "{why}");
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[150.0, 200.0, 260.0, 250.0]);
    // Locked: what it says may change, the rectangle may not; text that does not fit in it is refused.
    let many = "line\n".repeat(30);
    let doc = file("", &[&own("[10 200 190 250]", "/F 132")], |_| {});
    run(&doc, &set("hello there")).unwrap();
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[10.0, 200.0, 190.0, 250.0]);
    let why = run(&doc, &set(&many)).unwrap_err().to_string();
    assert!(why.contains("does not fit"), "{why}");
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[10.0, 200.0, 190.0, 250.0]);
    // Not locked, on the page: the box grows downwards.
    let doc = file("", &[&own("[10 200 190 250]", "")], |_| {});
    run(&doc, &set(&many)).unwrap();
    let r = nums(&doc, &dict(&doc, 10), "Rect");
    assert!(r[1] < 200.0 && (r[0], r[2], r[3]) == (10.0, 190.0, 250.0), "{r:?}");
}

#[test]
fn flags_are_read_one_way_an_integer_a_real_or_an_object() {
    let real = "<< /Type /Annot /Subtype /Square /Rect [10 10 60 60] /F 128.0 >>";
    let by_object = "<< /Type /Annot /Subtype /Square /Rect [70 10 120 60] /F 20 0 R >>";
    let doc = file("", &[real, by_object], |b| {
        b.obj(20, "132");
    });
    assert_eq!((modify::flags_of(&doc, &dict(&doc, 10)), modify::flags_of(&doc, &dict(&doc, 11))), (128, 132));
    for (i, num) in [(0, 10), (1, 11)] {
        let why = run(&doc, &Edit::Move { page: 0, index: i, num, delta: [5.0, 5.0] }).unwrap_err().to_string();
        assert!(why.contains("locked"), "{why}");
    }
}

#[test]
fn a_text_that_is_longer_than_the_reader_keeps_is_not_changed_here() {
    let long = format!("<< /Type /Annot /Subtype /Text /Rect [20 260 40 280] /F 28 /Contents ({}) >>", "y".repeat(4001));
    let exact = format!("<< /Type /Annot /Subtype /Text /Rect [50 260 70 280] /F 28 /Contents ({}) >>", "y".repeat(4000));
    let doc = file("", &[&long, &exact], |_| {});
    let why = run(&doc, &Edit::SetText { page: 0, index: 0, num: 10, text: "short".to_string() }).unwrap_err().to_string();
    assert!(why.contains("too long"), "{why}");
    run(&doc, &Edit::SetText { page: 0, index: 1, num: 11, text: "short".to_string() }).unwrap();
}

#[test]
fn endless_empty_strokes_and_a_point_that_goes_too_far_are_refused() {
    let strokes = |n: usize| format!("<< /Type /Annot /Subtype /Ink /Rect [10 70 110 120] /InkList [{}] >>", "[] ".repeat(n));
    let many = file("", &[&strokes(60_000)], |_| {});
    let why = run(&many, &Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 5.0] }).unwrap_err().to_string();
    assert!(why.contains("too many"), "{why}");
    run(&file("", &[&strokes(1_000)], |_| {}), &Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 5.0] }).unwrap();
    // A rectangle of less than a point has no scale to speak of.
    let thin = file("", &["<< /Type /Annot /Subtype /Square /Rect [10 10 10.5 60] >>"], |_| {});
    let why = run(&thin, &Edit::Resize { page: 0, index: 0, num: 10, rect: [10.0, 10.0, 150.0, 150.0] }).unwrap_err().to_string();
    assert!(why.contains("too small"), "{why}");
    // A vertex of 10^37 scaled up 100 times is beyond what a PDF holds; so is one of 10^39 that is only moved.
    let polygon = |at: &str| format!("<< /Type /Annot /Subtype /Polygon /Rect [0 0 2 2] /Vertices [0 0 {at} 5 1 1] >>");
    let far = file("", &[&polygon(&format!("1{}", "0".repeat(37)))], |_| {});
    let why = run(&far, &Edit::Resize { page: 0, index: 0, num: 10, rect: [0.0, 100.0, 200.0, 300.0] }).unwrap_err().to_string();
    assert!(why.contains("too far"), "{why}");
    run(&far, &Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 5.0] }).unwrap();
    let farther = file("", &[&polygon(&format!("1{}", "0".repeat(39)))], |_| {});
    let why = run(&farther, &Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 5.0] }).unwrap_err().to_string();
    assert!(why.contains("too far"), "{why}");
}

#[test]
fn the_replies_and_popups_of_an_annotation_move_with_it_and_a_locked_reply_stops_the_move() {
    let note = "<< /Type /Annot /Subtype /Text /Rect [20 260 40 280] /F 28 /Contents (a) /Popup 11 0 R >>";
    let popup = "<< /Type /Annot /Subtype /Popup /Rect [50 200 150 280] /Parent 10 0 R >>";
    let reply = "<< /Type /Annot /Subtype /Text /Rect [22 262 42 282] /F 28 /IRT 10 0 R /Popup 13 0 R /Contents (b) >>";
    let reply_popup = "<< /Type /Annot /Subtype /Popup /Rect [52 202 152 282] /Parent 12 0 R >>";
    let again = "<< /Type /Annot /Subtype /Text /Rect [24 264 44 284] /F 28 /IRT 12 0 R >>";
    let other = "<< /Type /Annot /Subtype /Text /Rect [100 100 120 120] /F 28 >>";
    let doc = file("", &[note, popup, reply, reply_popup, again, other], |_| {});
    // Ten points to the right and twenty down on the page as shown: user space has y up.
    run(&doc, &Edit::Move { page: 0, index: 0, num: 10, delta: [10.0, 20.0] }).unwrap();
    let want = [
        (10, [30.0, 240.0, 50.0, 260.0]),
        (11, [60.0, 180.0, 160.0, 260.0]),
        (12, [32.0, 242.0, 52.0, 262.0]),
        (13, [62.0, 182.0, 162.0, 262.0]),
        (14, [34.0, 244.0, 54.0, 264.0]),
        (15, [100.0, 100.0, 120.0, 120.0]),
    ];
    for (num, rect) in want {
        near(&nums(&doc, &dict(&doc, num), "Rect"), &rect);
    }
    // A locked reply stops the whole move: nothing is changed.
    let locked_reply = "<< /Type /Annot /Subtype /Text /Rect [22 262 42 282] /F 132 /IRT 10 0 R /Contents (b) >>";
    let doc = file("", &[note, popup, locked_reply], |_| {});
    let why = run(&doc, &Edit::Move { page: 0, index: 0, num: 10, delta: [10.0, 20.0] }).unwrap_err().to_string();
    assert!(why.contains("locked"), "{why}");
    near(&nums(&doc, &dict(&doc, 10), "Rect"), &[20.0, 260.0, 40.0, 280.0]);
    // The curves of a drawing in PDF 2.0 move too.
    let ink = "<< /Type /Annot /Subtype /Ink /Rect [10 70 110 120] /InkList [[10 70 110 120]] /Path [[10 70 60 90 110 120]] >>";
    let doc = file("", &[ink], |_| {});
    run(&doc, &Edit::Move { page: 0, index: 0, num: 10, delta: [5.0, 0.0] }).unwrap();
    let d = dict(&doc, 10);
    let Some(Object::Array(path)) = d.get("Path") else { panic!("no path") };
    let stroke: Vec<f64> = path[0].as_array().unwrap().iter().filter_map(Object::as_f64).collect();
    near(&stroke, &[15.0, 70.0, 65.0, 90.0, 115.0, 120.0]);
}
