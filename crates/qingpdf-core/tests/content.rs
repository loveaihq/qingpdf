//! Step 4c: text, pictures and watermarks written into the content of pages, through the engine's messages (the reader) and through
//! `edit::add_content` (the command line). Every saved file is checked as in `forms.rs` (original bytes a prefix, `qpdf --check`,
//! reopened); the pages are drawn by our own renderer and the marks looked for where they must be (upright, in the middle, at the
//! opacity asked for) on pages that are turned, whose boxes do not start at 0, whose resources are inherited or shared and whose
//! content is one stream, an array, shared or missing. The other readers are checked by `tests/tools/content_compare.py`, which
//! draws the files these tests leave in the folder named by `QINGPDF_CONTENT_OUT` (default `tests/out/content`).
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::Viewer;

use std::path::{Path, PathBuf};

use qingpdf_core::edit::{self, ContentOp, Watermark};
use qingpdf_core::object::{Object, ObjRef};
use qingpdf_core::text::TextExtractor;
use qingpdf_core::view::{ALL_PAGES, EditResult, EditStatus, Event, RenderRequest, Rendered, SaveStatus};
use qingpdf_core::Document;

impl Viewer {
    fn render(&mut self, page: u32) -> Rendered {
        let id = self.id();
        self.engine.render(RenderRequest { doc: 1, id, page, dpi: 72.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
        self.until(|e| match e {
            Event::Rendered(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn text(&mut self, page: u32, at: [f32; 2], text: &str, size: f32, color: [u8; 3]) -> EditResult {
        let id = self.id();
        self.engine.add_text(1, id, page, at, text, size, color);
        self.edited(id)
    }

    fn image(&mut self, page: u32, rect: [f32; 4], image: Vec<u8>) -> EditResult {
        let id = self.id();
        self.engine.add_image(1, id, page, rect, image);
        self.edited(id)
    }

    fn watermark(&mut self, mark: Watermark) -> EditResult {
        let id = self.id();
        self.engine.add_watermark(1, id, mark);
        self.edited(id)
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
        self.edited(id)
    }

    fn redo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.redo(1, id);
        self.edited(id)
    }

    fn save(&mut self, path: &Path) -> (SaveStatus, String, u64) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, bytes, .. } if i == id => Some((status, message, bytes)),
            _ => None,
        })
    }
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("QINGPDF_CONTENT_OUT").map(PathBuf::from).unwrap_or_else(|| common::workspace_root().join("tests").join("out").join("content"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file built by hand: the objects (number and text) and the trailer's extra entries.
fn pdf(objects: &[(u32, String)], trailer: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|o| o.0).max().unwrap() + 1;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None if n == 0 => out.extend_from_slice(b"0000000000 65535 f \n"),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R {trailer} >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

fn stream(body: &str) -> String {
    format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())
}

/// Five pages of every shape the content of a page can have: P1 has resources of its own and one stream (shared with P4); P2 is
/// turned 90 degrees, has resources that are an object (shared with P3) and its content in an array; P3 has boxes that do not start
/// at 0 (a crop box inside), a `/UserUnit`, is turned 270 degrees and has no content at all; P4 inherits its resources (shared with
/// P5) and shares P1's stream; P5's content is an array object. The middle of every page (as shown) has a black square.
fn shapes() -> Vec<u8> {
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R 5 0 R 8 0 R 9 0 R 10 0 R] /Count 5 /Resources 21 0 R /MediaBox [0 0 300 400] >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /Helv 30 0 R >> >> /Contents 4 0 R >>".to_string()),
        (4, stream("0 g 130 180 40 40 re f")),
        (5, "<< /Type /Page /Parent 2 0 R /Rotate 90 /Resources 20 0 R /Contents [6 0 R 7 0 R] >>".to_string()),
        (6, stream("BT /Helv 12 Tf 20 20 Td (hello) Tj ET")),
        (7, stream("0 g 130 180 40 40 re f")),
        (8, "<< /Type /Page /Parent 2 0 R /MediaBox [100 100 400 500] /CropBox [120 120 380 460] /UserUnit 2 /Rotate 270 /Resources 20 0 R >>".to_string()),
        (9, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_string()),
        (10, "<< /Type /Page /Parent 2 0 R /Contents 11 0 R >>".to_string()),
        (11, "[4 0 R 6 0 R]".to_string()),
        (20, "<< /Font << /Helv 30 0 R >> /ProcSet [/PDF /Text] >>".to_string()),
        (21, "<< /Font << /Helv 30 0 R >> >>".to_string()),
        (30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string()),
    ];
    pdf(&objects, "")
}

fn has_chinese_font() -> bool {
    ["msyh.ttc", "simsun.ttc", "simhei.ttf"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file())
}

/// A PNG of `w` by `h` pixels, all of one colour with this alpha.
fn flat_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgba.repeat((w * h) as usize)).unwrap();
    out
}

fn check_saved(original: &Path, saved: &Path, password: &str) {
    let before = std::fs::read(original).unwrap();
    let after = std::fs::read(saved).unwrap();
    assert!(after.len() > before.len(), "nothing was appended");
    assert!(after.starts_with(&before), "{}: the bytes of the original changed", saved.display());
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = if password.is_empty() { common::qpdf_check(&qpdf, saved) } else { common::qpdf_check_pw(&qpdf, saved, password) };
        let original_verdict = if password.is_empty() { common::qpdf_check(&qpdf, original) } else { common::qpdf_check_pw(&qpdf, original, password) };
        let rank = |code: i32| match code {
            0 => 0,
            3 => 1,
            _ => 2,
        };
        assert!(rank(verdict.code) <= rank(original_verdict.code), "{}: qpdf --check says {}:\n{}", saved.display(), verdict.code, verdict.text);
        let known = common::qpdf_complaints(&original_verdict.text, original);
        let new: Vec<String> = common::qpdf_complaints(&verdict.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
        assert!(new.is_empty(), "{}: qpdf warns of what the original did not: {new:?}\n{}", saved.display(), verdict.text);
    }
    Document::open_with_password(saved, password).unwrap();
}

/// The box (left, top, right, bottom, inclusive) of the pixels that `test` accepts, and how many there are.
fn ink_box(b: &Rendered, test: impl Fn([u8; 3]) -> bool) -> Option<([usize; 4], usize)> {
    let w = b.width as usize;
    let (mut found, mut count) = (None::<[usize; 4]>, 0);
    for (i, p) in b.bgra.as_chunks::<4>().0.iter().enumerate() {
        if test([p[2], p[1], p[0]]) {
            let (x, y) = (i % w, i / w);
            count += 1;
            found = Some(match found {
                Some([l, t, r, bt]) => [l.min(x), t.min(y), r.max(x), bt.max(y)],
                None => [x, y, x, y],
            });
        }
    }
    found.map(|f| (f, count))
}

fn pixel(b: &Rendered, x: usize, y: usize) -> [u8; 3] {
    let i = (y * b.width as usize + x) * 4;
    [b.bgra[i + 2], b.bgra[i + 1], b.bgra[i]]
}

fn red(c: [u8; 3]) -> bool {
    c[0] > 200 && c[1] < 60 && c[2] < 60
}

fn mark(text: &str, image: Vec<u8>, pages: Vec<u32>) -> Watermark {
    Watermark { text: text.to_string(), image, opacity: 1.0, angle: 0.0, tile: false, under: false, pages }
}

fn dict_of(doc: &Document, num: u32) -> qingpdf_core::Dict {
    match doc.get(ObjRef::new(num, 0)).unwrap() {
        Object::Dict(d) => d,
        other => panic!("object {num} is not a dictionary: {other:?}"),
    }
}

fn contents_of(doc: &Document, page: usize) -> Vec<u32> {
    let pages = doc.pages().unwrap();
    let d = dict_of(doc, pages[page].obj_ref.num);
    match d.get("Contents") {
        Some(Object::Array(a)) => a.iter().map(|o| o.as_obj_ref().unwrap().num).collect(),
        other => panic!("page {page}: /Contents is {other:?}"),
    }
}

fn stream_text(doc: &Document, num: u32) -> String {
    let Object::Stream(s) = doc.get(ObjRef::new(num, 0)).unwrap() else { panic!("object {num} is not a stream") };
    String::from_utf8_lossy(&doc.decode_stream(&s).unwrap()).into_owned()
}

#[test]
fn a_picture_watermark_is_upright_and_in_the_middle_of_every_shape_of_page_and_the_pages_are_wrapped_right() {
    let out = out_dir();
    let input = out.join("shapes.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let doc = Document::open(&input).unwrap();
    let before = {
        let mut v = Viewer::open(&input, "");
        (0..5).map(|p| v.render(p)).collect::<Vec<_>>()
    };
    // A red picture four times as wide as high, all of the way opaque, on all five pages.
    let op = ContentOp::Watermark(mark("", flat_png(80, 20, [255, 0, 0, 255]), (0..5).collect()));
    let added = edit::add_content(&doc, "", &op).unwrap();
    assert!(!added.rewritten);
    let saved = out.join("shapes-wm.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    check_saved(&input, &saved, "");

    let mut v = Viewer::open(&saved, "");
    for p in 0..5u32 {
        let b = v.render(p);
        let (bx, count) = ink_box(&b, red).unwrap_or_else(|| panic!("page {p}: no watermark"));
        let (w, h) = (b.width as f64, b.height as f64);
        let (cx, cy) = ((bx[0] + bx[2]) as f64 / 2.0 + 0.5, (bx[1] + bx[3]) as f64 / 2.0 + 0.5);
        assert!((cx - w / 2.0).abs() < 1.5 && (cy - h / 2.0).abs() < 1.5, "page {p}: the mark's middle is ({cx}, {cy}) on a page {w} x {h}");
        let aspect = (bx[2] - bx[0] + 1) as f64 / (bx[3] - bx[1] + 1) as f64;
        assert!((aspect - 4.0).abs() < 0.3, "page {p}: the mark is not upright (box {bx:?}, aspect {aspect})");
        assert!(count > 400, "page {p}: only {count} red pixels");
        // What was on the page is still there, away from the mark.
        let was = &before[p as usize];
        assert_eq!((was.width, was.height), (b.width, b.height), "page {p}: the size changed");
    }
    // The wrapping, page by page. P1: the shared stream between `q` and `Q ...`; P3 had no content: one stream; P2's array is kept whole.
    let after = Document::open(&saved).unwrap();
    let c1 = contents_of(&after, 0);
    assert_eq!((c1.len(), c1[1]), (3, 4), "{c1:?}");
    assert_eq!(stream_text(&after, c1[0]).trim(), "q");
    assert!(stream_text(&after, c1[2]).starts_with("Q\n") && stream_text(&after, c1[2]).contains(" Do Q"), "{}", stream_text(&after, c1[2]));
    let c2 = contents_of(&after, 1);
    assert_eq!(c2[1..3], [6, 7], "{c2:?}");
    assert_eq!(c2[0], c1[0], "the stream q is one object for all the pages");
    let c3 = contents_of(&after, 2);
    assert_eq!(c3.len(), 1, "{c3:?}");
    assert!(!stream_text(&after, c3[0]).starts_with("Q"), "nothing to close on a page that had no content");
    assert_eq!(contents_of(&after, 3)[1], 4, "P4 shares P1's stream, which is not changed");
    assert_eq!(contents_of(&after, 4)[1..3], [4, 6], "the array object of P5 is copied into the page, not changed");
    // The objects the pages shared are as they were: the array 11, the streams 4 and 6, the resources 20 and 21.
    for n in [4u32, 6, 11, 20, 21] {
        assert_eq!(after.get(ObjRef::new(n, 0)).unwrap(), doc.get(ObjRef::new(n, 0)).unwrap(), "object {n} was changed");
    }
    // The resources: P2 and P3 share the copy of 20; P4 and P5 the copy of 21; P1's own dictionary has the mark in place.
    let page_dict = |p: usize| dict_of(&after, after.pages().unwrap()[p].obj_ref.num);
    let r = |p: usize| page_dict(p).get("Resources").cloned().unwrap();
    assert!(matches!(r(0), Object::Dict(_)), "P1's resources are its own: {:?}", r(0));
    assert_eq!(r(1), r(2), "P2 and P3 shared object 20: they share its copy");
    assert_ne!(r(1), Object::Ref(ObjRef::new(20, 0)));
    assert_eq!(r(3), r(4), "P4 and P5 inherited the same resources");
    assert_ne!(r(3), Object::Ref(ObjRef::new(21, 0)));
    for p in 0..5 {
        let resources = match r(p) {
            Object::Ref(n) => dict_of(&after, n.num),
            Object::Dict(d) => d,
            other => panic!("{other:?}"),
        };
        let Some(Object::Dict(xobjects)) = resources.get("XObject") else { panic!("page {p}: no XObject in {resources:?}") };
        assert_eq!(xobjects.len(), 1, "page {p}");
    }
    // The fonts the pages had are still in their resources.
    let Object::Dict(own) = r(0) else { panic!() };
    assert!(own.get("Font").is_some(), "P1 lost its font");
    let Object::Ref(shared) = r(1) else { panic!() };
    assert!(dict_of(&after, shared.num).get("Font").is_some() && dict_of(&after, shared.num).get("ProcSet").is_some());
}

#[test]
fn text_goes_over_or_under_the_content_and_a_picture_at_a_place_with_its_opacity() {
    let out = out_dir();
    let input = out.join("shapes-2.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let doc = Document::open(&input).unwrap();
    let flat = flat_png(40, 40, [255, 0, 0, 255]);
    // Over the black square it shows red; under it, the square covers it.
    let over = edit::add_content(&doc, "", &ContentOp::Watermark(mark("", flat.clone(), vec![0]))).unwrap();
    let under = edit::add_content(&doc, "", &ContentOp::Watermark(Watermark { under: true, ..mark("", flat.clone(), vec![0]) })).unwrap();
    for (name, added, centre_red) in [("over", over, true), ("under", under, false)] {
        let path = out.join(format!("shapes-{name}.pdf"));
        std::fs::write(&path, &added.data).unwrap();
        check_saved(&input, &path, "");
        let mut v = Viewer::open(&path, "");
        let b = v.render(0);
        assert_eq!(red(pixel(&b, 150, 200)), centre_red, "{name}: the middle of the page is {:?}", pixel(&b, 150, 200));
        assert!(ink_box(&b, red).is_some(), "{name}: the mark shows round the square");
    }
    // Opacity: 30 per cent of red over white is (255, 179, 179).
    let faint = edit::add_content(&doc, "", &ContentOp::Watermark(Watermark { opacity: 0.3, ..mark("", flat.clone(), vec![0]) })).unwrap();
    let path = out.join("shapes-faint.pdf");
    std::fs::write(&path, &faint.data).unwrap();
    let b = Viewer::open(&path, "").render(0);
    let at = pixel(&b, 150, 175);
    assert!(at[0] > 250 && (170..=188).contains(&at[1]) && (170..=188).contains(&at[2]), "30 % red over white came out {at:?}");
    // A picture at a place: its lower left corner 40 points from the left and 100 from the bottom, 60 wide (the picture is square).
    let placed = edit::add_content(&doc, "", &ContentOp::Image { page: 1, at: [40.0, 100.0], width: 60.0, image: flat }).unwrap();
    let path = out.join("shapes-placed.pdf");
    std::fs::write(&path, &placed.data).unwrap();
    check_saved(&input, &path, "");
    let b = Viewer::open(&path, "").render(1);
    // P2 is turned 90 degrees: shown 400 wide and 300 high, so the box is x 40..100, y (300 - 100 - 60)..(300 - 100).
    let (bx, _) = ink_box(&b, red).unwrap();
    assert!(bx[0].abs_diff(40) <= 1 && bx[2].abs_diff(99) <= 1 && bx[1].abs_diff(140) <= 1 && bx[3].abs_diff(199) <= 1, "the picture is at {bx:?}");
}

#[test]
fn chinese_text_and_a_chinese_watermark_go_on_normal_turned_and_encrypted_files() {
    if !has_chinese_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    let out = out_dir();
    let plain = out.join("shapes-zh.pdf");
    std::fs::write(&plain, shapes()).unwrap();
    let mut cases: Vec<(PathBuf, String)> = vec![(plain.clone(), String::new())];
    if let Some(qpdf) = common::find_qpdf() {
        for (bits, name) in [("256", "aes256"), ("128", "aes128")] {
            let locked = out.join(format!("shapes-zh-{name}.pdf"));
            let aes = if bits == "128" { "--use-aes=y" } else { "--" };
            let status = std::process::Command::new(&qpdf).args(["--encrypt", "pw", "pw", bits, aes, "--", &plain.to_string_lossy(), &locked.to_string_lossy()]).status().unwrap();
            assert!(status.code().is_some_and(|c| c == 0 || c == 3));
            cases.push((locked, "pw".to_string()));
        }
    }
    for (input, password) in cases {
        let name = input.file_stem().unwrap().to_string_lossy().into_owned();
        let mut v = Viewer::open(&input, &password);
        // Text on page 1 (upright, from a point of the page as shown), on the turned page 2 and on page 3 (boxes, user unit, turn).
        for page in [0u32, 1, 2] {
            let r = v.text(page, [40.0, 100.0], "你好，世界\nHello 机密 2026", 20.0, [200, 0, 0]);
            assert_eq!(r.status, EditStatus::Done, "{name}: {}", r.message);
            assert_eq!(r.page, page);
        }
        let r = v.image(0, [20.0, 20.0, 120.0, 80.0], flat_png(40, 20, [0, 0, 255, 128]));
        assert_eq!(r.status, EditStatus::Done, "{name}: {}", r.message);
        // The watermark: tiled, turned 45 degrees, 30 per cent, over on the odd pages and under on the even ones.
        let all: Vec<u32> = (0..5).collect();
        let r = v.watermark(Watermark { opacity: 0.3, angle: 45.0, tile: true, ..mark("机密 CONFIDENTIAL", Vec::new(), all.clone()) });
        assert_eq!(r.status, EditStatus::Done, "{name}: {}", r.message);
        assert_eq!(r.page, ALL_PAGES);
        let drawn = (0..5).map(|p| v.render(p)).collect::<Vec<_>>();
        let saved = out.join(format!("{name}-marked.pdf"));
        let (status, message, _) = v.save(&saved);
        assert_eq!(status, SaveStatus::Done, "{name}: {message}");
        check_saved(&input, &saved, &password);

        // Opened again: drawn as the edited file was, and the text is there to be extracted (the font is ours, with a ToUnicode).
        let mut w = Viewer::open(&saved, &password);
        for p in 0..5u32 {
            assert_eq!(w.render(p).bgra, drawn[p as usize].bgra, "{name}: page {p} is drawn differently after saving");
        }
        let doc = Document::open_with_password(&saved, &password).unwrap();
        let pages = doc.pages().unwrap();
        let mut extractor = TextExtractor::new(&doc);
        for (page, p) in pages.iter().enumerate().take(3) {
            let text = extractor.page_text(p).unwrap();
            assert!(text.contains("你好，世界") && text.contains("Hello 机密 2026"), "{name}: page {page}: {text:?}");
        }
        // The watermark is an artifact of the page (14.8.2.2), not its text: the tiles are drawn and are not extracted.
        let tiled = extractor.page_text(&pages[3]).unwrap();
        assert!(!tiled.contains("CONFIDENTIAL") && !tiled.contains("机密"), "{name}: the tiles are extracted as text: {tiled:?}");
        // The watermark's 30 per cent grey shows on a page that is otherwise white (P4's only other content is a black square).
        assert!(ink_box(&drawn[3], |c| c[0] == c[1] && c[1] == c[2] && (180..250).contains(&c[0])).is_some_and(|(_, n)| n > 800), "{name}: no grey tiles");
    }
}

#[test]
fn undo_takes_a_watermark_off_every_page_and_redo_puts_it_back() {
    let out = out_dir();
    let input = out.join("shapes-undo.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let mut v = Viewer::open(&input, "");
    let plain: Vec<Rendered> = (0..5).map(|p| v.render(p)).collect();
    let r = v.watermark(mark("", flat_png(80, 20, [255, 0, 0, 255]), (0..5).collect()));
    assert_eq!((r.status, r.page, r.can_undo, r.dirty), (EditStatus::Done, ALL_PAGES, true, true), "{r:?}");
    let marked: Vec<Rendered> = (0..5).map(|p| v.render(p)).collect();
    assert!((0..5).all(|p| marked[p].bgra != plain[p].bgra && ink_box(&marked[p], red).is_some()));
    let r = v.undo();
    assert_eq!((r.status, r.page, r.dirty), (EditStatus::Done, ALL_PAGES, false), "{r:?}");
    for p in 0..5u32 {
        assert_eq!(v.render(p).bgra, plain[p as usize].bgra, "page {p} is not as it was after the undo");
    }
    let r = v.redo();
    assert_eq!((r.status, r.page, r.dirty), (EditStatus::Done, ALL_PAGES, true), "{r:?}");
    for p in 0..5u32 {
        assert_eq!(v.render(p).bgra, marked[p as usize].bgra, "page {p} is not marked again after the redo");
    }
    // Saved, undone past the save and saved again: both files whole.
    let first = out.join("shapes-undo-1.pdf");
    assert_eq!(v.save(&first).0, SaveStatus::Done);
    check_saved(&input, &first, "");
    assert_eq!(v.undo().status, EditStatus::Done);
    let second = out.join("shapes-undo-2.pdf");
    assert_eq!(v.save(&second).0, SaveStatus::Done);
    check_saved(&first, &second, "");
    assert!(ink_box(&Viewer::open(&second, "").render(0), red).is_none(), "the watermark is gone from the file saved after the undo");
}

#[test]
fn rights_and_signatures_decide_whether_the_content_may_be_changed() {
    let out = out_dir();
    let input = out.join("shapes-rights.pdf");
    std::fs::write(&input, shapes()).unwrap();
    // A file that does not allow changing its content (permission bit 4), made with qpdf: annotations are still allowed.
    if let Some(qpdf) = common::find_qpdf() {
        let locked = out.join("shapes-nomodify.pdf");
        let status = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner", "256", "--modify=none", "--annotate=y", "--", &input.to_string_lossy(), &locked.to_string_lossy()]).status().unwrap();
        assert!(status.code().is_some_and(|c| c == 0 || c == 3));
        let mut v = Viewer::open(&locked, "");
        assert!(!v.opened.rights.modify && v.opened.rights.annotate);
        let r = v.text(0, [10.0, 10.0], "x", 12.0, [0; 3]);
        assert_eq!(r.status, EditStatus::Denied, "{r:?}");
        assert!(r.message.contains("content"), "{}", r.message);
        assert_eq!(v.image(0, [10.0, 10.0, 50.0, 50.0], flat_png(4, 4, [0; 4])).status, EditStatus::Denied);
        assert_eq!(v.watermark(mark("secret", Vec::new(), vec![0])).status, EditStatus::Denied);
        // The command line says it too, and the owner password lifts it, in the reader and on the command line.
        let locked_doc = Document::open_with_password(&locked, "").unwrap();
        let op = ContentOp::Text { page: 0, at: [10.0, 10.0], size: 12.0, text: "x".to_string(), color: [0; 3] };
        let err = edit::add_content(&locked_doc, "", &op).unwrap_err().to_string();
        assert!(err.contains("permission bit 4") && err.contains("owner password"), "{err}");
        let owner_doc = Document::open_with_password(&locked, "owner").unwrap();
        let added = edit::add_content(&owner_doc, "owner", &op);
        assert!(added.is_ok(), "{:?}", added.err());
        let mut owner = Viewer::open(&locked, "owner");
        assert!(owner.opened.rights.modify);
        assert_eq!(owner.text(0, [10.0, 10.0], "x", 12.0, [0; 3]).status, EditStatus::Done);
    }
    // A signed file (a signature field with a signature in it), and certified ones (DocMDP 1, 2, 3): the content of a page is not changed,
    // whatever the level, and the file is not changed at all.
    let signed_catalog = |extra: &str, sig: &str| {
        pdf(
            &[
                (1, format!("<< /Type /Catalog /Pages 2 0 R {extra} >>")),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Annots [10 0 R] >>".to_string()),
                (10, "<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V 20 0 R >>".to_string()),
                (20, sig.to_string()),
            ],
            "",
        )
    };
    let signature = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>";
    let mut files = vec![("signed".to_string(), signed_catalog("/AcroForm << /Fields [10 0 R] /SigFlags 3 >>", signature))];
    for p in 1..=3 {
        let reference = format!("<< /Type /Sig /Reference [ << /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P {p} /V /1.2 >> >> ] >>");
        files.push((format!("certified-{p}"), signed_catalog("/Perms << /DocMDP 20 0 R >>", &reference)));
    }
    for (name, bytes) in files {
        let path = out.join(format!("{name}.pdf"));
        std::fs::write(&path, &bytes).unwrap();
        let mut v = Viewer::open(&path, "");
        for r in [v.text(0, [10.0, 10.0], "x", 12.0, [0; 3]), v.image(0, [10.0, 10.0, 50.0, 50.0], flat_png(4, 4, [0; 4])), v.watermark(mark("secret", Vec::new(), vec![0]))] {
            assert_eq!(r.status, EditStatus::Failed, "{name}: {r:?}");
            assert!(r.message.contains("signature") || r.message.contains("certification"), "{name}: {}", r.message);
            assert!(!r.dirty, "{name}");
        }
        let signed_doc = Document::open(&path).unwrap();
        let op = ContentOp::Watermark(mark("secret", Vec::new(), vec![0]));
        let err = edit::add_content(&signed_doc, "", &op).unwrap_err().to_string();
        assert!(err.contains("signed") || err.contains("certified"), "{name}: {err}");
        assert_eq!(v.save(&out.join(format!("{name}-saved.pdf"))).0, SaveStatus::Done);
        assert_eq!(std::fs::read(out.join(format!("{name}-saved.pdf"))).unwrap(), bytes, "{name}: nothing was changed, so nothing is written");
    }
    // A signature field with nothing in it is not a signature.
    let unsigned = signed_catalog("/AcroForm << /Fields [10 0 R] >>", "<< /Type /Sig >>");
    let path = out.join("unsigned-field.pdf");
    std::fs::write(&path, &unsigned).unwrap();
    assert!(Document::open(&path).is_ok());
}

#[test]
fn bad_input_is_refused_and_leaves_the_file_alone() {
    let out = out_dir();
    let input = out.join("shapes-bad.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let mut v = Viewer::open(&input, "");
    let fine = flat_png(4, 4, [255, 0, 0, 255]);
    let failed = |r: EditResult, what: &str| {
        assert_eq!(r.status, EditStatus::Failed, "{what}: {r:?}");
        assert!(!r.dirty, "{what}: the file is as it was");
    };
    // Text: none, no size, no place, too many lines, a page that is not there.
    failed(v.text(0, [10.0, 10.0], " \n ", 12.0, [0; 3]), "blank text");
    failed(v.text(0, [10.0, 10.0], "x", f32::NAN, [0; 3]), "size");
    failed(v.text(0, [f32::NAN, 10.0], "x", 12.0, [0; 3]), "place");
    failed(v.text(0, [10.0, 1.0e9], "x", 12.0, [0; 3]), "far place");
    failed(v.text(0, [10.0, 10.0], &"x\n".repeat(2_500), 12.0, [0; 3]), "2500 lines");
    failed(v.text(9, [10.0, 10.0], "x", 12.0, [0; 3]), "page 9");
    // Pictures: none, cut, no area, a place that is not a number, one that claims 25 million pixels.
    failed(v.image(0, [10.0, 10.0, 50.0, 50.0], b"not a picture".to_vec()), "not a picture");
    failed(v.image(0, [10.0, 10.0, 50.0, 50.0], fine[..30].to_vec()), "cut picture");
    failed(v.image(0, [10.0, 10.0, 10.0, 50.0], fine.clone()), "no area");
    failed(v.image(0, [f32::NAN, 10.0, 50.0, 50.0], fine.clone()), "place");
    let mut big = fine.clone();
    big[16..20].copy_from_slice(&5000u32.to_be_bytes());
    big[20..24].copy_from_slice(&5000u32.to_be_bytes());
    failed(v.image(0, [10.0, 10.0, 50.0, 50.0], big), "5000 x 5000 pixels");
    // Watermarks: nothing to show, too long a text, opacity 0 or over 1 or not a number, an angle that is not a number, no page, a
    // page that is not there, too many pages, a picture that is none.
    failed(v.watermark(mark("  ", Vec::new(), vec![0])), "nothing");
    failed(v.watermark(mark(&"x".repeat(201), Vec::new(), vec![0])), "201 characters");
    for opacity in [0.0, -1.0, 1.5, f32::NAN] {
        failed(v.watermark(Watermark { opacity, ..mark("x", Vec::new(), vec![0]) }), "opacity");
    }
    failed(v.watermark(Watermark { angle: f32::INFINITY, ..mark("x", Vec::new(), vec![0]) }), "angle");
    failed(v.watermark(mark("x", Vec::new(), Vec::new())), "no pages");
    failed(v.watermark(mark("x", Vec::new(), vec![0, 7])), "page 7");
    failed(v.watermark(mark("x", Vec::new(), vec![0; edit::MAX_WATERMARK_PAGES + 1])), "too many pages");
    failed(v.watermark(mark("", b"not a picture".to_vec(), vec![0])), "not a picture");
    // A page named twice is done once; and still nothing is the matter with the file.
    let r = v.watermark(mark("", fine, vec![0, 0, 1, 0]));
    assert_eq!(r.status, EditStatus::Done, "{r:?}");
    let saved = out.join("shapes-bad-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&input, &saved, "");
    let section = String::from_utf8_lossy(&std::fs::read(&saved).unwrap()[std::fs::read(&input).unwrap().len()..]).into_owned();
    assert_eq!(section.matches("/Type /Page ").count(), 2, "two pages were changed, one of them named three times: {section}");
}

/// A page of 200 x 200 with this `/Contents` and `/Resources`, for the files of the tests below.
fn small_page(n: u32, contents: &str, resources: &str) -> (u32, String) {
    (n, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {contents} /Resources {resources} >>"))
}

#[test]
fn the_new_content_is_drawn_in_the_state_the_page_started_in_whatever_the_page_leaves_open() {
    let out = out_dir();
    // P1 leaves two `q` open (a scale in the first); P2 starts with a stray `Q` and then scales; P3 ends inside a text object; P4 ends inside a
    // path; P5 is balanced (the control); P6 closes twice what it opened. Each fills with blue (the watermark is red).
    let fonts = "<< /Font << /F1 30 0 R >> >>";
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R 6 0 R 9 0 R 11 0 R 13 0 R 15 0 R] /Count 6 >>".to_string()),
        small_page(3, "[4 0 R 5 0 R]", "<< >>"),
        (4, stream("q 0.25 0 0 0.25 0 0 cm")),
        (5, stream("q 0 0 1 rg 0 0 400 400 re f")),
        small_page(6, "[7 0 R 8 0 R]", "<< >>"),
        (7, stream("Q 0.5 0 0 0.5 0 0 cm")),
        (8, stream("0 0 1 rg 0 0 400 400 re f")),
        small_page(9, "10 0 R", fonts),
        (10, stream("BT /F1 12 Tf 10 10 Td (hi) Tj")),
        small_page(11, "12 0 R", "<< >>"),
        (12, stream("0 0 1 RG 10 10 m 190 190 l")),
        small_page(13, "14 0 R", "<< >>"),
        (14, stream("q 0.5 0 0 0.5 0 0 cm 0 0 1 rg 0 0 400 400 re f Q")),
        small_page(15, "[16 0 R 17 0 R]", "<< >>"),
        (16, stream("q q 0.5 0 0 0.5 0 0 cm")),
        (17, stream("0 0 1 rg 0 0 400 400 re f Q Q Q Q 2 0 0 2 0 0 cm")),
        (30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()),
    ];
    let input = out.join("open-states.pdf");
    std::fs::write(&input, pdf(&objects, "")).unwrap();
    let doc = Document::open(&input).unwrap();
    let added = edit::add_content(&doc, "", &ContentOp::Watermark(mark("", flat_png(80, 20, [255, 0, 0, 255]), (0..6).collect()))).unwrap();
    let saved = out.join("open-states-wm.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    check_saved(&input, &saved, "");
    let mut v = Viewer::open(&saved, "");
    for p in 0..6u32 {
        let b = v.render(p);
        let (bx, count) = ink_box(&b, red).unwrap_or_else(|| panic!("page {p}: no watermark"));
        let (w, h) = (b.width as f64, b.height as f64);
        let (cx, cy) = ((bx[0] + bx[2]) as f64 / 2.0 + 0.5, (bx[1] + bx[3]) as f64 / 2.0 + 0.5);
        assert!((cx - w / 2.0).abs() < 1.5 && (cy - h / 2.0).abs() < 1.5, "page {p}: the mark's middle is ({cx}, {cy}) on a page {w} x {h} (box {bx:?})");
        assert!(count > 400, "page {p}: only {count} red pixels");
    }
    // What was made up for, page by page: the `q` in front, and what closes after the page's own streams.
    let after = Document::open(&saved).unwrap();
    let words = |text: String| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let shape = |page: usize| {
        let c = contents_of(&after, page);
        (words(stream_text(&after, c[0])), words(stream_text(&after, *c.last().unwrap()).split("/Artifact").next().unwrap().to_string()))
    };
    assert_eq!(shape(0), ("q".to_string(), "Q Q Q".to_string()), "two `q` left open: three `Q`");
    assert_eq!(shape(1), ("q q".to_string(), "Q".to_string()), "one stray `Q`: two `q` in front");
    assert_eq!(shape(2), ("q".to_string(), "ET Q".to_string()), "a text object left open is ended");
    assert_eq!(shape(3), ("q".to_string(), "n Q".to_string()), "a path left pending is ended");
    assert_eq!(shape(4), ("q".to_string(), "Q".to_string()), "a balanced page needs one pair");
    assert_eq!(shape(5), ("q q q".to_string(), "Q".to_string()), "two stray `Q` after two `q` left open");
    // The streams that are the same are one object: the pages that need one `q` share it.
    assert_eq!(contents_of(&after, 2)[0], contents_of(&after, 4)[0]);
    assert_ne!(contents_of(&after, 0)[0], contents_of(&after, 1)[0]);
}

#[test]
fn pages_that_share_big_resources_or_an_xobject_dictionary_share_one_copy_and_the_cost_does_not_grow_with_their_size() {
    let out = out_dir();
    // 3000 pages inherit one direct `/Resources` with 20000 fonts (cloned for each page, that took seconds), and 400 pages each
    // have resources of their own whose `/XObject` is one object they all share.
    let fonts: String = (0..20_000).map(|i| format!("/F{i} 5 0 R ")).collect();
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()),
        (6, "<< /Fm0 7 0 R >>".to_string()),
        (7, "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] >>".to_string()),
    ];
    let kids: String = (0..3_400).map(|i| format!("{} 0 R ", 100 + i)).collect();
    objects.push((2, format!("<< /Type /Pages /Kids [{kids}] /Count 3400 /MediaBox [0 0 200 200] /Resources << /Font << {fonts} >> >> >>")));
    for i in 0..3_000 {
        objects.push((100 + i, "<< /Type /Page /Parent 2 0 R >>".to_string()));
    }
    for i in 3_000..3_400 {
        objects.push((100 + i, "<< /Type /Page /Parent 2 0 R /Resources << /XObject 6 0 R /ProcSet [/PDF] >> >>".to_string()));
    }
    let input = out.join("shared-big.pdf");
    std::fs::write(&input, pdf(&objects, "")).unwrap();
    let doc = Document::open(&input).unwrap();
    let started = std::time::Instant::now();
    let op = ContentOp::Watermark(mark("", flat_png(8, 8, [255, 0, 0, 255]), (0..3_400).collect()));
    let added = edit::add_content(&doc, "", &op).unwrap();
    let took = started.elapsed();
    if !cfg!(debug_assertions) {
        assert!(took.as_secs_f64() < 3.0, "it took {took:?}");
    }
    // One copy of the 20000 fonts, for all 3000 pages; the file grew by little more than that and the pages' own dictionaries.
    let grew = added.data.len() - std::fs::metadata(&input).unwrap().len() as usize;
    assert!(grew < 2_000_000, "the update takes {grew} bytes");
    let saved = out.join("shared-big-wm.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    let after = Document::open(&saved).unwrap();
    let pages = after.pages().unwrap();
    let resources_of = |i: usize| dict_of(&after, pages[i].obj_ref.num).get("Resources").cloned().unwrap();
    let first = resources_of(0);
    assert!(matches!(first, Object::Ref(_)), "{first:?}");
    assert!((0..3_000).all(|i| resources_of(i) == first), "the pages that inherit the same resources share its copy");
    // The 400 pages with resources of their own: the `/XObject` is one new object, with the old entry and the mark; 6 is as it was.
    let xobject_of = |i: usize| match resources_of(i) {
        Object::Dict(d) => d.get("XObject").cloned().unwrap(),
        other => panic!("{other:?}"),
    };
    let shared = xobject_of(3_000);
    let Object::Ref(copy) = shared else { panic!("the /XObject of a page with resources of its own is {shared:?}") };
    assert!(copy.num > 3_500 && (3_000..3_400).all(|i| xobject_of(i) == shared));
    let Object::Dict(entries) = after.get(copy).unwrap() else { panic!() };
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries.get("Fm0").is_some());
    assert_eq!(after.get(ObjRef::new(6, 0)).unwrap(), doc.get(ObjRef::new(6, 0)).unwrap());
}

#[test]
fn only_a_watermark_is_marked_as_an_artifact_and_only_a_watermark_artifact_is_left_out_of_the_text() {
    let out = out_dir();
    let input = out.join("artifacts.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let doc = Document::open(&input).unwrap();
    let marked = "/Artifact <</Type /Pagination /Subtype /Watermark>> BDC";
    // Text added to the page is its content; a watermark (of text, and of a picture) is an artifact, every tile inside one sequence.
    let text = edit::add_content(&doc, "", &ContentOp::Text { page: 0, at: [20.0, 100.0], size: 14.0, text: "Added Words".to_string(), color: [0; 3] }).unwrap();
    let text_file = out.join("artifacts-text.pdf");
    std::fs::write(&text_file, &text.data).unwrap();
    let text_doc = Document::open(&text_file).unwrap();
    let c = contents_of(&text_doc, 0);
    assert!(!c.iter().any(|&n| stream_text(&text_doc, n).contains("BDC")), "added text is not marked");
    assert!(TextExtractor::new(&text_doc).page_text(&text_doc.pages().unwrap()[0]).unwrap().contains("Added Words"));
    for (name, watermark) in [("text", mark("SECRET", Vec::new(), vec![0, 1])), ("picture", mark("", flat_png(8, 8, [255, 0, 0, 255]), vec![0, 1]))] {
        let watermark = Watermark { tile: true, ..watermark };
        let made = edit::add_content(&doc, "", &ContentOp::Watermark(watermark)).unwrap();
        let file = out.join(format!("artifacts-{name}.pdf"));
        std::fs::write(&file, &made.data).unwrap();
        let after = Document::open(&file).unwrap();
        let last = *contents_of(&after, 0).last().unwrap();
        let body = stream_text(&after, last);
        assert!(body.contains(marked) && body.trim_end().ends_with("EMC") && body.matches("BDC").count() == 1 && body.matches("EMC").count() == 1, "{name}: {body}");
        assert!(body.matches(" Do Q").count() > 3, "{name}: the tiles are in one sequence: {body}");
        let mut extractor = TextExtractor::new(&after);
        for page in after.pages().unwrap().iter().take(2) {
            let text = extractor.page_text(page).unwrap();
            assert!(!text.contains("SECRET"), "{name}: {text:?}");
        }
        check_saved(&input, &file, "");
    }
    // The extractor: a header artifact, a span that merely carries a `/Subtype /Watermark`, and a watermark artifact (with a nested
    // sequence, and one drawn by a form); text after the watermark and after a form that left a sequence open is text again.
    let page = "BT /F1 12 Tf 20 150 Td (Body one) Tj ET\n\
        /Artifact <</Type /Pagination /Subtype /Header>> BDC BT /F1 12 Tf 20 130 Td (Header) Tj ET EMC\n\
        /Span <</Subtype /Watermark>> BDC BT /F1 12 Tf 20 110 Td (Span) Tj ET EMC\n\
        /Artifact <</Type /Pagination\n/Subtype\n/Watermark>> BDC /P BMC BT /F1 12 Tf 20 90 Td (Hidden) Tj ET EMC BT /F1 12 Tf 20 70 Td (Hidden too) Tj ET /Fm0 Do EMC\n\
        /Artifact <</Subtype /WatermarkX>> BDC BT /F1 12 Tf 20 50 Td (Close name) Tj ET EMC\n\
        /Fm1 Do BT /F1 12 Tf 20 30 Td (Body two) Tj ET";
    let form = |body: &str| format!("<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 30 0 R >> >> /Length {} >>\nstream\n{body}\nendstream", body.len());
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        small_page(3, "4 0 R", "<< /Font << /F1 30 0 R >> /XObject << /Fm0 5 0 R /Fm1 6 0 R >> >>"),
        (4, stream(page)),
        (5, form("BT /F1 12 Tf 20 10 Td (Form hidden) Tj ET")),
        (6, form("/Artifact <</Subtype /Watermark>> BDC BT /F1 12 Tf 120 130 Td (Left open) Tj ET")),
        (30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string()),
    ];
    let file = out.join("artifacts-hand.pdf");
    std::fs::write(&file, pdf(&objects, "")).unwrap();
    let hand = Document::open(&file).unwrap();
    let got = TextExtractor::new(&hand).page_text(&hand.pages().unwrap()[0]).unwrap();
    for shown in ["Body one", "Header", "Span", "Close name", "Body two"] {
        assert!(got.contains(shown), "{shown} is text: {got:?}");
    }
    for hidden in ["Hidden", "Form hidden", "Left open"] {
        assert!(!got.contains(hidden), "{hidden} is a watermark: {got:?}");
    }
}

#[test]
fn a_watermark_goes_on_25000_pages_and_the_room_it_may_take_is_what_is_left() {
    let out = out_dir();
    // 25000 bare pages through the command line's path, then the reader's.
    let kids: String = (0..25_000).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let mut objects = vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()), (2, format!("<< /Type /Pages /Kids [{kids}] /Count 25000 /MediaBox [0 0 300 400] >>"))];
    objects.extend((0..25_000).map(|i| (10 + i, "<< /Type /Page /Parent 2 0 R >>".to_string())));
    let input = out.join("bare-25000.pdf");
    std::fs::write(&input, pdf(&objects, "")).unwrap();
    let doc = Document::open(&input).unwrap();
    let started = std::time::Instant::now();
    let op = ContentOp::Watermark(mark("", flat_png(8, 8, [255, 0, 0, 255]), (0..25_000).collect()));
    let added = edit::add_content(&doc, "", &op).unwrap();
    if !cfg!(debug_assertions) {
        assert!(started.elapsed().as_secs_f64() < 5.0, "it took {:?}", started.elapsed());
    }
    let saved = out.join("bare-25000-wm.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    assert_eq!(Document::open(&saved).unwrap().page_count().unwrap(), 25_000);
    let mut v = Viewer::open(&input, "");
    let r = v.watermark(mark("", flat_png(8, 8, [255, 0, 0, 255]), (0..25_000).collect()));
    assert_eq!(r.status, EditStatus::Done, "{r:?}");

    // The reader sets memory aside for what is not saved yet (16 MiB): 2000 pages with 10 KB in each page dictionary, rewritten, take
    // 20 MB. An edit of all of them says it is too big without telling the person to save (nothing is unsaved); one of 1000 fits; the
    // next one only has what is left, and then it does say to save first.
    let junk = "x".repeat(10_000);
    let kids: String = (0..2_000).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let mut heavy = vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()), (2, format!("<< /Type /Pages /Kids [{kids}] /Count 2000 /MediaBox [0 0 100 100] >>"))];
    heavy.extend((0..2_000).map(|i| (10 + i, format!("<< /Type /Page /Parent 2 0 R /Junk ({junk}) >>"))));
    let input = out.join("heavy-2000.pdf");
    std::fs::write(&input, pdf(&heavy, "")).unwrap();
    let mut v = Viewer::open(&input, "");
    let picture = flat_png(8, 8, [255, 0, 0, 255]);
    let all = v.watermark(mark("", picture.clone(), (0..2_000).collect()));
    assert_eq!(all.status, EditStatus::Failed, "{all:?}");
    assert!(all.message.contains("too big") && !all.message.contains("save the file first"), "{}", all.message);
    let half = v.watermark(mark("", picture.clone(), (0..1_000).collect()));
    assert_eq!(half.status, EditStatus::Done, "{half:?}");
    let more = v.watermark(mark("", picture.clone(), (1_000..2_000).collect()));
    assert_eq!(more.status, EditStatus::Failed, "{more:?}");
    assert!(more.message.contains("too big") && more.message.contains("save the file first"), "{}", more.message);
    // After the save there is room again.
    let saved = out.join("heavy-2000-wm.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let again = v.watermark(mark("", picture, (1_000..2_000).collect()));
    assert_eq!(again.status, EditStatus::Done, "{again:?}");
}
