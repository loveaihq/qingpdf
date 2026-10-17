//! Shapes, and moving, resizing, recolouring and giving a text to annotations that are already on a page (2026-10-17), through the engine's
//! messages. Every saved file is checked the way the 4a files are: the bytes of the original are an exact prefix, one cross-reference section
//! is added, `qpdf --check` accepts it, our reader opens it again and draws it the way it was drawn before the save. The other readers
//! (PDFium, MuPDF) are checked by `tests/tools/annot_edit_compare.py`, which reads the `*.json` files these tests leave in `tests/out/annot_edit`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use common::Viewer;

use std::path::{Path, PathBuf};
use std::time::Instant;

use qingpdf_core::Document;
use qingpdf_core::view::{AnnotInfo, EditResult, EditStatus, Event, ListedAnnot, MarkupKind, RenderRequest, Rendered, SaveStatus, ShapeKind, keep_on_page, line_rects};

impl Viewer {
    fn boxes(&mut self, page: u32) -> Vec<[f32; 4]> {
        let id = self.id();
        self.engine.char_boxes(1, id, page);
        self.until(|e| match e {
            Event::CharBoxes(b) if b.id == id => Some(b.boxes),
            _ => None,
        })
    }

    fn annotations(&mut self, page: u32) -> Vec<AnnotInfo> {
        let id = self.id();
        self.engine.annotations(1, id, page);
        self.until(|e| match e {
            Event::Annotations { id: i, items, .. } if i == id => Some(items),
            _ => None,
        })
    }

    fn list(&mut self) -> (Vec<ListedAnnot>, bool) {
        let id = self.id();
        self.engine.annot_list(1, id);
        self.until(|e| match e {
            Event::AnnotList { id: i, items, truncated, .. } if i == id => Some((items, truncated)),
            _ => None,
        })
    }

    fn text_of(&mut self, page: u32, a: &AnnotInfo) -> (bool, String) {
        let id = self.id();
        self.engine.annotation_text(1, id, page, a.index, a.num);
        self.until(|e| match e {
            Event::AnnotText { id: i, found, text, .. } if i == id => Some((found, text)),
            _ => None,
        })
    }

    fn render(&mut self, page: u32) -> Rendered {
        let id = self.id();
        self.engine.render(RenderRequest { doc: 1, id, page, dpi: 72.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
        self.until(|e| match e {
            Event::Rendered(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn save(&mut self, path: &Path) -> (SaveStatus, String, u64) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, bytes, .. } if i == id => Some((status, message, bytes)),
            _ => None,
        })
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
        self.edited(id)
    }

    fn shape(&mut self, page: u32, kind: ShapeKind, from: [f32; 2], to: [f32; 2], color: [u8; 3], width: f32, fill: bool) -> EditResult {
        let id = self.id();
        self.engine.add_shape(1, id, page, kind, from, to, color, width, fill, "Tester");
        self.edited(id)
    }

    fn mv(&mut self, page: u32, a: &AnnotInfo, delta: [f32; 2]) -> EditResult {
        let id = self.id();
        self.engine.move_annotation(1, id, page, a.index, a.num, delta);
        self.edited(id)
    }

    fn resize(&mut self, page: u32, a: &AnnotInfo, rect: [f32; 4]) -> EditResult {
        let id = self.id();
        self.engine.resize_annotation(1, id, page, a.index, a.num, rect);
        self.edited(id)
    }

    fn restyle(&mut self, page: u32, a: &AnnotInfo, color: Option<[u8; 3]>, width: Option<f32>, fill: Option<bool>) -> EditResult {
        let id = self.id();
        self.engine.restyle_annotation(1, id, page, a.index, a.num, color, width, fill);
        self.edited(id)
    }

    fn retext(&mut self, page: u32, a: &AnnotInfo, text: &str) -> EditResult {
        let id = self.id();
        self.engine.set_annotation_text(1, id, page, a.index, a.num, text);
        self.edited(id)
    }

    fn delete(&mut self, page: u32, a: &AnnotInfo) -> EditResult {
        let id = self.id();
        self.engine.delete_annotation(1, id, page, a.index, a.num);
        self.edited(id)
    }
}

/// A refusal that is the reader's way of not losing anything (what it cannot draw again, a text box that does not fit or hangs over the page), or of
/// not trusting a file (a point too far out, a font that is missing): not a failure when the annotation comes from another program.
fn on_purpose(message: &str) -> bool {
    ["no installed font", "cannot draw again", "not draw this kind", "edge of the page", "does not fit", "too long", "too far", "too small", "too many", "cannot be read", "is empty"].iter().any(|m| message.contains(m))
}

fn corpus(rel: &str) -> PathBuf {
    common::public_file(rel)
}

fn local(rel: &str) -> Option<PathBuf> {
    let path = common::corpus_root().join("local").join(rel);
    path.is_file().then_some(path)
}

fn out_dir() -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("annot_edit");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The rectangle of changed pixels between two drawings of a page at 72 dpi, in points (`None` if they are the same).
fn changed_box(before: &Rendered, after: &Rendered) -> Option<[f32; 4]> {
    assert_eq!((before.width, before.height), (after.width, after.height));
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..before.height {
        for x in 0..before.width {
            let i = ((y * before.width + x) * 4) as usize;
            let differs = (0..3).any(|k| before.bgra[i + k].abs_diff(after.bgra[i + k]) > 12);
            if differs {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    (x1 >= x0 && y1 >= y0).then_some([x0 as f32, y0 as f32, (x1 + 1) as f32, (y1 + 1) as f32])
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let (x0, y0, x1, y1) = (a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3]));
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

fn near(a: [f32; 4], b: [f32; 4], slack: f32, what: &str) {
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= slack), "{what}: {a:?} is not {b:?}");
}

/// What every saved file must satisfy: the original is a prefix, one section was added, qpdf accepts it (`password` for encrypted files), and our
/// reader opens it.
fn check_saved(original: &Path, saved: &Path, password: &str) {
    let before = std::fs::read(original).unwrap();
    let after = std::fs::read(saved).unwrap();
    assert!(after.len() > before.len(), "{}: nothing was appended", saved.display());
    assert!(after.starts_with(&before), "{}: the bytes of the original changed", saved.display());
    let count = |b: &[u8]| b.windows(9).filter(|w| *w == b"startxref").count();
    assert_eq!(count(&after), count(&before) + 1, "{}: not one section more", saved.display());
    if let Some(qpdf) = common::find_qpdf() {
        let check = |p: &Path| if password.is_empty() { common::qpdf_check(&qpdf, p) } else { common::qpdf_check_pw(&qpdf, p, password) };
        let (verdict, was) = (check(saved), check(original));
        let rank = |code: i32| match code {
            0 => 0,
            3 => 1,
            _ => 2,
        };
        assert!(rank(verdict.code) <= rank(was.code), "{}: qpdf --check says {}:\n{}", saved.display(), verdict.code, verdict.text);
        let known = common::qpdf_complaints(&was.text, original);
        let new: Vec<String> = common::qpdf_complaints(&verdict.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
        assert!(new.is_empty(), "{}: qpdf warns of what the original did not: {new:?}\n{}", saved.display(), verdict.text);
    }
    Document::open_with_password(saved, password).unwrap();
}

/// A record for `annot_edit_compare.py`: `base` and `saved` drawn by the other readers differ in these boxes (points from the top left) and
/// in these colours; a `tight` box is where the difference ends (a shape fills its rectangle), the others only hold it (a text box's
/// rectangle is bigger than its text).
fn manifest(name: &str, base: &Path, saved: &Path, password: &str, page: u32, boxes: &[([f32; 4], Option<[u8; 3]>, bool)]) {
    let path = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let list: Vec<String> = boxes
        .iter()
        .map(|(b, c, tight)| {
            let colour = c.map_or("null".to_string(), |c| format!("[{},{},{}]", c[0], c[1], c[2]));
            format!(r#"{{"box":[{},{},{},{}],"colour":{colour},"tight":{tight}}}"#, b[0], b[1], b[2], b[3])
        })
        .collect();
    let json = format!(r#"{{"name":"{name}","base":"{}","saved":"{}","password":"{password}","page":{page},"boxes":[{}]}}"#, path(base), path(saved), list.join(","));
    std::fs::write(out_dir().join(format!("{name}.json")), json).unwrap();
}

fn text_page(v: &mut Viewer) -> (u32, Vec<[f32; 4]>) {
    for page in 0..v.opened.pages.len().min(8) as u32 {
        let boxes = v.boxes(page);
        if boxes.iter().filter(|b| b[2] > b[0] && b[3] > b[1]).count() >= 20 {
            return (page, line_rects(&boxes[..boxes.len().min(60)]));
        }
    }
    panic!("no page with text");
}

fn one(list: &[AnnotInfo], subtype: &str) -> AnnotInfo {
    list.iter().rev().find(|a| a.subtype == subtype).unwrap_or_else(|| panic!("no {subtype} in {:?}", list.iter().map(|a| &a.subtype).collect::<Vec<_>>())).clone()
}

/// A PNG of `w` by `h` pixels, half see-through red.
fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        let data: Vec<u8> = (0..w * h).flat_map(|_| [220, 20, 20, 200]).collect();
        writer.write_image_data(&data).unwrap();
    }
    out
}

// --- shapes ----------------------------------------------------------------------------------------------------------

fn shapes_on(name: &str, input: &Path, password: &str) {
    let mut v = Viewer::open(input, password);
    assert!(v.opened.rights.annotate);
    let page = 0;
    let size = v.opened.pages[0];
    let (w, h) = (size.width as f32, size.height as f32);
    let mut last = v.render(page);
    let existing = v.annotations(page).len();
    // (Places as parts of the page, so that small pages do as well as big ones.)
    let at = |x: f32, y: f32| [(x * w).round(), (y * h).round()];
    let shapes = [
        (ShapeKind::Rectangle, at(0.10, 0.10), at(0.40, 0.20), [0u8, 0, 255], 4.0, false),
        (ShapeKind::Ellipse, at(0.55, 0.10), at(0.85, 0.20), [0, 160, 0], 2.0, true),
        (ShapeKind::Line, at(0.10, 0.30), at(0.40, 0.40), [255, 0, 0], 3.0, false),
        (ShapeKind::Arrow, at(0.55, 0.40), at(0.85, 0.30), [0, 0, 0], 3.0, false),
    ];
    for (kind, from, to, color, width, fill) in shapes {
        let r = v.shape(page, kind, from, to, color, width, fill);
        assert_eq!(r.status, EditStatus::Done, "{name} {kind:?}: {}", r.message);
        let drawn = v.render(page);
        let moved = changed_box(&last, &drawn).unwrap_or_else(|| panic!("{name}: {kind:?} changed nothing"));
        let (l, t, r2, b) = (from[0].min(to[0]), from[1].min(to[1]), from[0].max(to[0]), from[1].max(to[1]));
        let pad = width / 2.0 + 14.0;
        assert!(moved[0] >= l - pad && moved[1] >= t - pad && moved[2] <= r2 + pad && moved[3] <= b + pad, "{name}: {kind:?} drew outside its place: {moved:?}");
        last = drawn;
    }
    // The page lists them where they were drawn.
    let list = v.annotations(page);
    assert_eq!(list.len(), existing + 4);
    let rect = one(&list, "Square");
    let (a, b) = (shapes[0].1, shapes[0].2);
    near(rect.rect, [a[0] - 2.0, a[1] - 2.0, b[0] + 2.0, b[1] + 2.0], 0.01, "the rectangle's box is the rectangle grown by half the pen");
    assert_eq!(rect.author, "Tester");
    let arrow = one(&list, "Line");
    assert!(arrow.line.is_some());
    let saved = out_dir().join(format!("shapes-{name}.pdf"));
    let (status, message, _) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    check_saved(input, &saved, password);
    // Our reader draws the saved file as it was drawn.
    let mut again = Viewer::open(&saved, password);
    assert_eq!(changed_box(&v.render(page), &again.render(page)), None, "{name}: the saved file is drawn differently");
    // (The ellipse is filled: most of what is drawn there is the colour lightened, 70 % of the way to white.)
    let expected: Vec<([f32; 4], Option<[u8; 3]>, bool)> = list[existing..].iter().zip([[0u8, 0, 255], [179, 227, 179], [255, 0, 0], [0, 0, 0]]).map(|(a, c)| (a.rect, Some(c), true)).collect();
    manifest(&format!("shapes-{name}"), input, &saved, password, page, &expected);
}

#[test]
fn shapes_on_a_classic_file() {
    shapes_on("classic", &corpus("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf"), "");
}

#[test]
fn shapes_on_a_file_with_a_cross_reference_stream() {
    shapes_on("xref-stream", &corpus("zh/gongwen/gongwen-2024-nicheng-work-report.pdf"), "");
}

#[test]
fn shapes_on_a_hybrid_file() {
    shapes_on("hybrid", &corpus("zh/lunwen/lunwen-arxiv-2403.14268-word-tc.pdf"), "");
}

#[test]
fn shapes_on_an_aes256_file_in_object_streams() {
    shapes_on("aes256", &corpus("encrypted/qpdf-generated/objstm.r6-aes256-empty-extract-n.pdf"), "");
    shapes_on("aes256-chinese", &corpus("encrypted/qpdf-generated/vertical.r6-aes256-user-chinese.pdf"), "\u{5BC6}\u{7801}");
}

#[test]
fn changes_to_annotations_work_in_encrypted_files_and_the_new_parts_are_encrypted() {
    // AES-256 (revision 6) with the empty password, and AES-128 and RC4 with a password: shapes made, then moved, resized and recoloured.
    for (file, password) in [
        ("encrypted/qpdf-generated/bookmarks.r6-aes256-empty-print-none.pdf", ""),
        ("encrypted/qpdf-generated/vertical.r6-aes256-user-chinese.pdf", "\u{5BC6}\u{7801}"),
        ("encrypted/qpdf-generated/bookmarks.r4-aes128-user-assemble-n.pdf", "user"),
        ("encrypted/qpdf-generated/bookmarks.r3-128-rc4-user-modify-none.pdf", "owner"),
    ] {
        let input = corpus(file);
        let mut v = Viewer::open(&input, password);
        assert!(v.opened.rights.annotate, "{file}");
        let size = v.opened.pages[0];
        let (w, h) = (size.width as f32, size.height as f32);
        let at = |x: f32, y: f32| [(x * w).round(), (y * h).round()];
        assert_eq!(v.shape(0, ShapeKind::Rectangle, at(0.1, 0.1), at(0.4, 0.2), [0, 0, 255], 3.0, false).status, EditStatus::Done);
        assert_eq!(v.shape(0, ShapeKind::Ellipse, at(0.5, 0.1), at(0.8, 0.2), [0, 160, 0], 2.0, false).status, EditStatus::Done);
        let list = v.annotations(0);
        let (rect, circle) = (one(&list, "Square"), one(&list, "Circle"));
        assert_eq!(v.mv(0, &rect, [10.0, 10.0]).status, EditStatus::Done, "{file}");
        assert_eq!(v.resize(0, &circle, [0.5 * w, 0.1 * h, 0.9 * w, 0.3 * h]).status, EditStatus::Done, "{file}");
        assert_eq!(v.restyle(0, &rect, Some([255, 0, 0]), Some(5.0), Some(true)).status, EditStatus::Done, "{file}");
        let want = v.annotations(0);
        let saved = out_dir().join(format!("changed-enc-{}", file.rsplit('/').next().unwrap()));
        let (status, message, _) = v.save(&saved);
        assert_eq!(status, SaveStatus::Done, "{file}: {message}");
        check_saved(&input, &saved, password);
        assert!(Document::open_with_password(&saved, password).unwrap().is_encrypted(), "{file}");
        let mut again = Viewer::open(&saved, password);
        let reread = again.annotations(0);
        assert_eq!(reread.len(), want.len());
        for (a, b) in reread.iter().zip(&want) {
            near(a.rect, b.rect, 0.01, &format!("{file}: the saved file has the annotation where it was"));
        }
        assert_eq!(changed_box(&v.render(0), &again.render(0)), None, "{file}: the saved file is drawn differently");
    }
}

// --- annotations of ours, changed -------------------------------------------------------------------------------------

#[test]
fn our_markup_drawing_text_box_picture_and_shape_are_moved_resized_and_recoloured() {
    let input = corpus("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf");
    let mut v = Viewer::open(&input, "");
    let (page, lines) = text_page(&mut v);
    let base = v.render(page);
    let existing = v.annotations(page).len() as u32;
    let size = v.opened.pages[page as usize];
    let (pw, ph) = (size.width as f32, size.height as f32);
    // One of each kind.
    assert_eq!(v.engine_markup(page, lines[0]).status, EditStatus::Done);
    let id = v.id();
    v.engine.add_ink(1, id, page, vec![vec![[60.0, 300.0], [100.0, 340.0], [140.0, 300.0]]], [255, 0, 255], 3.0, "Tester");
    assert_eq!(v.edited(id).status, EditStatus::Done);
    let id = v.id();
    v.engine.add_freetext(1, id, page, [200.0, 300.0, 330.0, 340.0], "move me \u{79FB}\u{52A8}", 14.0, [0, 0, 0], "Tester");
    assert_eq!(v.edited(id).status, EditStatus::Done);
    let id = v.id();
    v.engine.add_stamp(1, id, page, [60.0, 400.0, 140.0, 440.0], png_bytes(40, 20), "Tester");
    assert_eq!(v.edited(id).status, EditStatus::Done);
    assert_eq!(v.shape(page, ShapeKind::Rectangle, [200.0, 400.0], [300.0, 450.0], [0, 0, 255], 3.0, false).status, EditStatus::Done);

    let list = v.annotations(page);
    assert_eq!(list.len() as u32, existing + 5);
    let (hl, ink, text, stamp, rect) = (one(&list, "Highlight"), one(&list, "Ink"), one(&list, "FreeText"), one(&list, "Stamp"), one(&list, "Square"));
    let mut expected: Vec<([f32; 4], Option<[u8; 3]>, bool)> = Vec::new();

    // Moved: the rectangle follows exactly, whatever the page.
    let r = v.mv(page, &hl, [20.0, 30.0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.mv(page, &ink, [25.0, 35.0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let list = v.annotations(page);
    let (hl2, ink2) = (one(&list, "Highlight"), one(&list, "Ink"));
    near(hl2.rect, [hl.rect[0] + 20.0, hl.rect[1] + 30.0, hl.rect[2] + 20.0, hl.rect[3] + 30.0], 0.02, "the highlight moved");
    near(ink2.rect, [ink.rect[0] + 25.0, ink.rect[1] + 35.0, ink.rect[2] + 25.0, ink.rect[3] + 35.0], 0.02, "the drawing moved");
    assert_eq!(hl2.parts.len(), hl.parts.len(), "the lines of the highlight went with it");
    near(hl2.parts[0], [hl.parts[0][0] + 20.0, hl.parts[0][1] + 30.0, hl.parts[0][2] + 20.0, hl.parts[0][3] + 30.0], 0.02, "the first line of the highlight moved");

    // Recoloured: the colour of what is drawn changes, the rectangle does not.
    let r = v.restyle(page, &hl2, Some([0, 160, 255]), None, None);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.restyle(page, &ink2, Some([0, 160, 0]), Some(6.0), None);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.restyle(page, &rect, Some([255, 0, 0]), Some(2.0), Some(true));
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);

    // Resized: a text box gets more room (its text keeps its size), the picture keeps its shape, the rectangle is drawn again.
    let r = v.resize(page, &text, [200.0, 300.0, 420.0, 400.0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.resize(page, &stamp, [60.0, 400.0, 220.0, 480.0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.resize(page, &rect, [200.0, 400.0, 380.0, 500.0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let list = v.annotations(page);
    let (text2, stamp2, rect2) = (one(&list, "FreeText"), one(&list, "Stamp"), one(&list, "Square"));
    near(rect2.rect, [200.0, 400.0, 380.0, 500.0], 0.02, "the rectangle is where it was put");
    near(stamp2.rect, [60.0, 400.0, 220.0, 480.0], 0.02, "the picture is where it was put");
    assert!(text2.rect[2] - text2.rect[0] > 200.0 && (text2.rect[0] - 200.0).abs() < 0.02, "{:?}", text2.rect);
    // A new text for the text box, typed over the old (two lines).
    let (found, old) = v.text_of(page, &text2);
    assert!(found && old == "move me \u{79FB}\u{52A8}", "{old:?}");
    let r = v.retext(page, &text2, "new text\nsecond line \u{65B0}\u{6587}\u{5B57}");
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let list = v.annotations(page);
    let text3 = one(&list, "FreeText");
    let (_, full) = v.text_of(page, &text3);
    assert_eq!(full, "new text\nsecond line \u{65B0}\u{6587}\u{5B57}");
    // The text box is recoloured (the text is red now).
    let r = v.restyle(page, &text3, Some([230, 30, 30]), None, None);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);

    // Each is drawn where its rectangle says (our own drawing; the other readers are checked by the script).
    let list = v.annotations(page);
    let drawn = v.render(page);
    let changed = changed_box(&base, &drawn).expect("something is drawn");
    let union = list[existing as usize..].iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |a, i| [a[0].min(i.rect[0]), a[1].min(i.rect[1]), a[2].max(i.rect[2]), a[3].max(i.rect[3])]);
    assert!(changed[0] >= union[0] - 2.0 && changed[1] >= union[1] - 2.0 && changed[2] <= union[2] + 2.0 && changed[3] <= union[3] + 2.0, "{changed:?} is not inside {union:?}");
    assert!(pw > 0.0 && ph > 0.0);
    for (subtype, colour) in [("Highlight", [0u8, 160, 255]), ("Ink", [0, 160, 0]), ("Square", [255, 0, 0]), ("FreeText", [230, 30, 30]), ("Stamp", [220, 20, 20])] {
        expected.push((one(&list, subtype).rect, Some(colour), subtype != "FreeText"));
    }
    // Saved: one section, and the other readers draw it the same (checked by the script).
    let saved = out_dir().join("ours-changed.pdf");
    let (status, message, _) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    check_saved(&input, &saved, "");
    let mut again = Viewer::open(&saved, "");
    assert_eq!(changed_box(&v.render(page), &again.render(page)), None, "the saved file is drawn differently");
    assert_eq!(again.annotations(page).len(), list.len());
    manifest("ours-changed", &input, &saved, "", page, &expected);
    // Undone, the changes are taken back one by one, and the original is what is left of the page.
    let mut undone = 0;
    while v.undo().status == EditStatus::Done {
        undone += 1;
    }
    assert_eq!(undone, 5 + 2 + 3 + 3 + 1 + 1, "every edit is one step");
    assert_eq!(v.annotations(page).len() as u32, existing);
    assert_eq!(changed_box(&base, &v.render(page)), None);
    let _ = (hl2, ink2);
}

impl Viewer {
    fn engine_markup(&mut self, page: u32, rect: [f32; 4]) -> EditResult {
        let id = self.id();
        self.engine.add_markup(1, id, page, MarkupKind::Highlight, vec![rect], [255, 235, 0], "Tester");
        self.edited(id)
    }
}

// --- annotations of other programs, changed --------------------------------------------------------------------------

#[test]
fn annotations_made_by_other_programs_are_moved_resized_and_recoloured() {
    let files: [(&str, &str); 6] = [
        ("incremental/comments.pdf", "comments"),
        ("linearized/linearized.pdf", "linearized"),
        ("cjk/issue20504.pdf", "freetext-cjk"),
        ("xref-stream-objstm/issue17492.pdf", "freetext-objstm"),
        ("xref-stream-objstm/bug1811510.pdf", "freetext-objstm2"),
        ("incremental/annotation-highlight-without-appearance.pdf", "highlight-no-ap"),
    ];
    let mut checked = 0;
    for (rel, name) in files {
        let Some(input) = local(rel) else {
            println!("SKIPPED {rel}: not in the local corpus");
            continue;
        };
        let mut probe = Viewer::open(&input, "");
        // The first page that has annotations of the kinds we look at.
        let Some((page, list)) = (0..probe.opened.pages.len().min(40) as u32).map(|p| (p, probe.annotations(p))).find(|(_, l)| l.iter().any(|a| !a.locked())) else { continue };
        let size = probe.opened.pages[page as usize];
        let base = probe.render(page);
        println!("{name}: page {page} has {:?}", list.iter().map(|a| a.subtype.as_str()).collect::<Vec<_>>());
        // At most three annotations of different kinds from each file.
        let mut seen: Vec<String> = Vec::new();
        for a in list.iter().filter(|a| !a.locked()) {
            if seen.contains(&a.subtype) || seen.len() >= 3 {
                continue;
            }
            seen.push(a.subtype.clone());
            let tag = format!("{name}-{}-{}", a.subtype.to_lowercase(), a.index);
            let fresh = |edit: &dyn Fn(&mut Viewer, &AnnotInfo) -> EditResult, suffix: &str| -> Option<(PathBuf, AnnotInfo, Viewer)> {
                let mut v = Viewer::open(&input, "");
                let r = edit(&mut v, a);
                if r.status == EditStatus::Failed && on_purpose(&r.message) {
                    println!("{tag} {suffix}: refused on purpose, {}", r.message);
                    return None;
                }
                assert_eq!(r.status, EditStatus::Done, "{tag} {suffix}: {}", r.message);
                let saved = out_dir().join(format!("{tag}-{suffix}.pdf"));
                let (status, message, _) = v.save(&saved);
                assert_eq!(status, SaveStatus::Done, "{tag} {suffix}: {message}");
                check_saved(&input, &saved, "");
                let after = v.annotations(page).into_iter().find(|x| x.index == a.index).expect("still there");
                Some((saved, after, v))
            };
            // The baseline: the annotation taken out. What is drawn of it later is what it adds to this.
            let (deleted, _, _) = {
                let mut v = Viewer::open(&input, "");
                let r = v.delete(page, a);
                assert_eq!(r.status, EditStatus::Done, "{tag} delete: {}", r.message);
                let saved = out_dir().join(format!("{tag}-deleted.pdf"));
                assert_eq!(v.save(&saved).0, SaveStatus::Done);
                (saved, (), v)
            };
            let mut deleted_viewer = Viewer::open(&deleted, "");
            let without = deleted_viewer.render(page);
            let original_box = changed_box(&without, &base);
            // Moved by (40, 25) points.
            let Some((moved_file, after, mut moved_viewer)) = fresh(&|v, a| v.mv(page, a, [40.0, 25.0]), "moved") else { continue };
            let (dx, dy) = keep_on_page(a.rect.map(f64::from), (40.0, 25.0), (size.width, size.height));
            let want = [a.rect[0] + dx as f32, a.rect[1] + dy as f32, a.rect[2] + dx as f32, a.rect[3] + dy as f32];
            near(after.rect, want, 0.02, &format!("{tag}: moved"));
            assert_eq!(after.parts.len(), a.parts.len(), "{tag}: the parts of the annotation");
            let mut expected = vec![(after.rect, None, false)];
            if let (Some(drawn), Some(was)) = (changed_box(&without, &moved_viewer.render(page)), original_box) {
                // Drawn at the new place (the box of what is drawn moved by what the annotation moved by).
                let shifted = [was[0] + dx as f32, was[1] + dy as f32, was[2] + dx as f32, was[3] + dy as f32];
                assert!(iou(drawn, shifted) > 0.7, "{tag}: drawn at {drawn:?}, expected near {shifted:?} (it was at {was:?})");
            } else {
                // No appearance: our reader draws nothing. The others make a mark on text from its quadrilaterals (so they show that those
                // moved too), and nothing else.
                println!("{tag}: nothing of it is drawn (no appearance)");
                if !matches!(a.subtype.as_str(), "Highlight" | "Underline" | "StrikeOut" | "Squiggly") {
                    expected.clear();
                }
            }
            if !expected.is_empty() {
                manifest(&format!("{tag}-moved"), &deleted, &moved_file, "", page, &expected);
            }
            // Resized to one and a half times its width and height, if that kind is.
            if a.resizable() {
                let grown = [a.rect[0], a.rect[1], (a.rect[0] + (a.rect[2] - a.rect[0]) * 1.5).min(size.width as f32 - 1.0), (a.rect[1] + (a.rect[3] - a.rect[1]) * 1.5).min(size.height as f32 - 1.0)];
                if let Some((resized_file, after, mut viewer)) = fresh(&|v, a| v.resize(page, a, grown), "resized") {
                    assert!(after.rect[2] - after.rect[0] > (a.rect[2] - a.rect[0]) * 1.2, "{tag}: it did not grow: {:?} from {:?}", after.rect, a.rect);
                    if let Some(drawn) = changed_box(&without, &viewer.render(page)) {
                        // (The text of a text box is as wide as it was in a box that is wider.)
                        assert!(a.subtype == "FreeText" || drawn[2] - drawn[0] > (a.rect[2] - a.rect[0]) * 1.1, "{tag}: what is drawn did not grow: {drawn:?}");
                        manifest(&format!("{tag}-resized"), &deleted, &resized_file, "", page, &[(after.rect, None, false)]);
                    }
                }
            }
            // Recoloured, if the reader draws that kind.
            if let Some((colour_file, after, mut viewer)) = a.restylable().then(|| fresh(&|v, a| v.restyle(page, a, Some([0, 0, 255]), None, None), "blue")).flatten() {
                let drawn = changed_box(&without, &viewer.render(page)).unwrap_or_else(|| panic!("{tag}: recoloured, nothing is drawn"));
                assert!(drawn[0] >= after.rect[0] - 3.0 && drawn[2] <= after.rect[2] + 3.0 && drawn[1] >= after.rect[1] - 3.0 && drawn[3] <= after.rect[3] + 3.0, "{tag}: drawn outside its rectangle: {drawn:?} / {:?}", after.rect);
                manifest(&format!("{tag}-blue"), &deleted, &colour_file, "", page, &[(after.rect, if a.subtype == "FreeText" { None } else { Some([0, 0, 255]) }, false)]);
            }
            checked += 1;
        }
    }
    println!("{checked} annotations of other programs moved, resized and recoloured");
    if local("incremental/comments.pdf").is_some() {
        assert!(checked >= 3, "only {checked} annotations were tried");
    }
}

// --- locks, undo, one section ----------------------------------------------------------------------------------------

fn synthetic(name: &str, annots: &[String]) -> PathBuf {
    use qingpdf_base::testutil::PdfBuilder;
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    let refs: String = (0..annots.len()).map(|i| format!("{} 0 R ", 10 + i)).collect();
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 500] /Resources << >> /Annots [{refs}] >>"));
    for (i, a) in annots.iter().enumerate() {
        b.obj(10 + i as u32, a);
    }
    let path = out_dir().join(format!("{name}.pdf"));
    std::fs::write(&path, b.finish_classic(10 + annots.len() as u32 + 1, "/Root 1 0 R")).unwrap();
    path
}

#[test]
fn a_locked_annotation_is_refused_everything_and_one_whose_text_is_locked_keeps_its_text() {
    let path = synthetic(
        "locks",
        &[
            "<< /Type /Annot /Subtype /Square /Rect [20 400 120 480] /C [1 0 0] /F 132 >>".to_string(),
            "<< /Type /Annot /Subtype /Text /Rect [200 400 220 420] /F 516 /Contents (kept) >>".to_string(),
            "<< /Type /Annot /Subtype /Circle /Rect [20 300 120 380] /C [0 0 1] /F 4 >>".to_string(),
        ],
    );
    let mut v = Viewer::open(&path, "");
    let list = v.annotations(0);
    assert_eq!(list.len(), 3);
    let (locked, text_locked, free) = (list[0].clone(), list[1].clone(), list[2].clone());
    assert!(locked.locked() && text_locked.text_locked() && !text_locked.locked() && !free.locked());
    for r in [
        v.mv(0, &locked, [5.0, 5.0]),
        v.resize(0, &locked, [20.0, 20.0, 150.0, 150.0]),
        v.restyle(0, &locked, Some([0, 0, 0]), None, None),
        v.delete(0, &locked),
        v.retext(0, &text_locked, "changed"),
    ] {
        assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
        assert!(r.message.contains("locked"), "{}", r.message);
        assert!(!r.dirty && !r.can_undo, "a refused edit changes nothing");
    }
    // Locked does not restrict what the annotation says (Table 165).
    assert_eq!(v.retext(0, &locked, "allowed").status, EditStatus::Done);
    // The contents-locked note can still be moved and recoloured, and the free circle can be changed in every way.
    assert_eq!(v.mv(0, &text_locked, [5.0, 5.0]).status, EditStatus::Done);
    assert_eq!(v.restyle(0, &text_locked, Some([0, 0, 255]), None, None).status, EditStatus::Done);
    assert_eq!(v.mv(0, &free, [5.0, 5.0]).status, EditStatus::Done);
    assert_eq!(v.resize(0, &free, [20.0, 300.0, 200.0, 400.0]).status, EditStatus::Done);
    assert_eq!(v.restyle(0, &free, Some([0, 160, 0]), Some(5.0), Some(true)).status, EditStatus::Done);
    let (_, text) = v.text_of(0, &text_locked);
    assert_eq!(text, "kept");
    // Undo: the six that were done, and then nothing.
    for _ in 0..6 {
        assert_eq!(v.undo().status, EditStatus::Done);
    }
    assert_eq!(v.undo().status, EditStatus::Nothing);
    // A file that says so is not changed by an annotation tool: a read-only (bit 7) annotation is like a locked one, and its text is kept too.
    let path = synthetic("readonly", &["<< /Type /Annot /Subtype /Square /Rect [20 400 120 480] /F 68 >>".to_string()]);
    let mut v = Viewer::open(&path, "");
    let a = v.annotations(0)[0].clone();
    assert!(a.locked() && a.text_locked());
    assert_eq!(v.mv(0, &a, [5.0, 5.0]).status, EditStatus::Failed);
    assert_eq!(v.retext(0, &a, "x").status, EditStatus::Failed);
}

#[test]
fn a_text_that_is_too_long_to_be_typed_over_is_not_offered_and_not_changed() {
    let long = "y".repeat(4001);
    let path = synthetic(
        "long-text",
        &[
            format!("<< /Type /Annot /Subtype /Text /Rect [20 400 40 420] /F 28 /Contents ({long}) >>"),
            format!("<< /Type /Annot /Subtype /FreeText /Rect [60 300 300 360] /DA (0 0 0 rg /Helv 12 Tf) /Border [0 0 0] /Contents ({long}) >>"),
            format!("<< /Type /Annot /Subtype /Text /Rect [50 400 70 420] /F 28 /Contents ({}) >>", "y".repeat(4000)),
        ],
    );
    let mut v = Viewer::open(&path, "");
    let list = v.annotations(0);
    assert_eq!(list.len(), 3);
    let (note, boxed, exact) = (list[0].clone(), list[1].clone(), list[2].clone());
    assert!(!v.text_of(0, &note).0 && !v.text_of(0, &boxed).0, "the first 4000 characters would be shown and the rest lost on saving");
    let (found, text) = v.text_of(0, &exact);
    assert!(found && text.chars().count() == 4000);
    for a in [&note, &boxed] {
        let r = v.retext(0, a, "short");
        assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    }
    assert!(!v.undo().can_undo, "nothing was changed");
}

#[test]
fn the_list_of_a_document_stops_looking_after_a_million_and_a_half_entries() {
    // 10,000 pages that share one array of 20,000 links: 200 million entries if each page were read.
    use qingpdf_base::testutil::PdfBuilder;
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..10_000).map(|p| format!("{} 0 R ", 100 + p)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 10000 >>"));
    b.obj(3, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>");
    b.obj(4, &format!("[{}]", "3 0 R ".repeat(20_000)));
    for p in 0..10_000u32 {
        b.obj(100 + p, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Annots 4 0 R >>");
    }
    let path = out_dir().join("shared-annots.pdf");
    std::fs::write(&path, b.finish_classic(10_200, "/Root 1 0 R")).unwrap();
    let mut v = Viewer::open(&path, "");
    let started = Instant::now();
    let (items, truncated) = v.list();
    let took = started.elapsed();
    println!("the list of 10,000 pages that share 20,000 links: {took:?}");
    assert!(items.is_empty() && truncated, "{} listed, truncated: {truncated}", items.len());
    assert!(took.as_secs() < 30, "{took:?}");
}

#[test]
fn a_file_that_forbids_annotating_refuses_all_of_it_without_the_owner_password() {
    let input = corpus("encrypted/qpdf-generated/two.r6-aes256-user-everything-denied.pdf");
    let mut v = Viewer::open(&input, "user");
    assert!(!v.opened.rights.annotate);
    let r = v.shape(0, ShapeKind::Rectangle, [20.0, 20.0], [80.0, 80.0], [255, 0, 0], 2.0, false);
    assert_eq!(r.status, EditStatus::Denied);
    let a = AnnotInfo { index: 0, num: 1, subtype: "Square".to_string(), rect: [0.0; 4], parts: Vec::new(), line: None, contents: String::new(), author: String::new(), modified: String::new(), flags: 0 };
    assert_eq!(v.mv(0, &a, [1.0, 1.0]).status, EditStatus::Denied);
    assert_eq!(v.resize(0, &a, [1.0, 1.0, 50.0, 50.0]).status, EditStatus::Denied);
    assert_eq!(v.restyle(0, &a, Some([0, 0, 0]), None, None).status, EditStatus::Denied);
    assert_eq!(v.retext(0, &a, "x").status, EditStatus::Denied);
    // With the owner password the same shape goes in, and the file keeps its encryption.
    let mut v = Viewer::open(&input, "owner");
    assert!(v.opened.rights.annotate);
    let r = v.shape(0, ShapeKind::Ellipse, [20.0, 20.0], [80.0, 80.0], [255, 0, 0], 2.0, false);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let saved = out_dir().join("owner-shape.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&input, &saved, "owner");
    assert!(Document::open_with_password(&saved, "owner").unwrap().is_encrypted());
}

#[test]
fn a_certified_file_allows_changes_to_annotations_only_with_p_3() {
    use qingpdf_base::testutil::PdfBuilder;
    for (p, allowed) in [(1, false), (2, false), (3, true)] {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 5 0 R >> >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R] >>");
        b.obj(4, "<< /Type /Annot /Subtype /Square /Rect [20 20 120 120] /C [1 0 0] >>");
        b.obj(5, &format!("<< /Type /Sig /Reference [ << /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P {p} /V /1.2 >> >> ] >>"));
        let path = out_dir().join(format!("docmdp-{p}.pdf"));
        std::fs::write(&path, b.finish_classic(10, "/Root 1 0 R")).unwrap();
        let mut v = Viewer::open(&path, "");
        let a = v.annotations(0)[0].clone();
        let (shape, moved) = (v.shape(0, ShapeKind::Line, [20.0, 20.0], [100.0, 100.0], [0, 0, 0], 2.0, false), v.mv(0, &a, [3.0, 3.0]));
        let want = if allowed { EditStatus::Done } else { EditStatus::Failed };
        assert_eq!((shape.status, moved.status), (want, want), "P {p}: {} {}", shape.message, moved.message);
        if !allowed {
            assert!(shape.message.contains("DocMDP") || shape.message.contains("certified"), "{}", shape.message);
        }
    }
}

// --- speed ---------------------------------------------------------------------------------------------------------------

#[test]
fn five_hundred_annotations_on_a_page_are_listed_and_moved_quickly_and_five_thousand_are_listed_in_200_milliseconds() {
    // 500 squares on one page.
    let squares: Vec<String> = (0..500u32).map(|i| format!("<< /Type /Annot /Subtype /Square /Rect [{} {} {} {}] /C [1 0 0] /F 4 /Contents (n{i}) >>", 10 + (i % 25) * 15, 10 + (i / 25) * 22, 22 + (i % 25) * 15, 28 + (i / 25) * 22)).collect();
    let path = synthetic("annots-500", &squares);
    let mut v = Viewer::open(&path, "");
    let started = Instant::now();
    let list = v.annotations(0);
    let listed = started.elapsed();
    assert_eq!(list.len(), 500);
    let started = Instant::now();
    let r = v.mv(0, &list[250], [7.0, 7.0]);
    let moved = started.elapsed();
    assert_eq!(r.status, EditStatus::Done);
    let started = Instant::now();
    let r = v.restyle(0, &list[251], Some([0, 0, 255]), Some(3.0), Some(true));
    let restyled = started.elapsed();
    assert_eq!(r.status, EditStatus::Done);
    println!("500 annotations on a page: listed in {listed:?}, one moved in {moved:?}, one recoloured in {restyled:?}");
    if !cfg!(debug_assertions) {
        assert!(moved.as_millis() < 16 && restyled.as_millis() < 16 && listed.as_millis() < 16, "{listed:?} {moved:?} {restyled:?}");
    }
    // 5,000 annotations, 100 on each of 50 pages, of several kinds.
    use qingpdf_base::testutil::PdfBuilder;
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..50).map(|p| format!("{} 0 R ", 3 + p)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 50 >>"));
    let mut next = 100u32;
    for p in 0..50u32 {
        let mut refs = String::new();
        for k in 0..100u32 {
            let (x, y) = (10 + (k % 10) * 30, 10 + (k / 10) * 40);
            let body = match k % 4 {
                0 => format!("<< /Type /Annot /Subtype /Text /Rect [{x} {y} {} {}] /F 28 /Contents (note {p}-{k}) /T (Author {k}) /M (D:2026101{}1200Z) >>", x + 20, y + 20, k % 9),
                1 => format!("<< /Type /Annot /Subtype /Square /Rect [{x} {y} {} {}] /C [1 0 0] /T (Author {k}) /M (D:20261001120000Z) /Contents (square {p}-{k}) >>", x + 25, y + 30),
                2 => format!("<< /Type /Annot /Subtype /Highlight /Rect [{x} {y} {} {}] /QuadPoints [{x} {} {} {} {x} {y} {} {y}] /Contents (mark {k}) >>", x + 25, y + 12, y + 12, x + 25, y + 12, x + 25),
                _ => format!("<< /Type /Annot /Subtype /Popup /Rect [{x} {y} {} {}] >>", x + 25, y + 30),
            };
            b.obj(next, &body);
            refs.push_str(&format!("{next} 0 R "));
            next += 1;
        }
        b.obj(3 + p, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 500] /Resources << >> /Annots [{refs}] >>"));
    }
    let path = out_dir().join("annots-5000.pdf");
    std::fs::write(&path, b.finish_classic(next + 1, "/Root 1 0 R")).unwrap();
    let mut v = Viewer::open(&path, "");
    let started = Instant::now();
    let (items, truncated) = v.list();
    let took = started.elapsed();
    // (A quarter of them are popups, which are not listed.)
    assert_eq!(items.len(), 3750);
    assert!(!truncated);
    assert!(items.windows(2).all(|w| (w[0].page, w[0].info.index) < (w[1].page, w[1].info.index)), "in page order, then the order of the page's list");
    assert_eq!((items[0].info.modified.as_str(), items[0].info.author.as_str()), ("2026-10-10", "Author 0"));
    println!("the list of 5,000 annotations (3,750 listed): {took:?}");
    if !cfg!(debug_assertions) {
        assert!(took.as_millis() <= 200, "{took:?}");
    }
}

// --- every file of the corpus --------------------------------------------------------------------------------------------

#[test]
fn a_shape_can_be_drawn_moved_resized_and_recoloured_in_every_file_of_the_corpus_that_opens() {
    let out = common::fresh_out_dir("annot-edit-corpus");
    let qpdf = common::find_qpdf();
    let (mut done, mut skipped, mut quirks, mut touched) = (0usize, 0usize, 0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();
    for (n, input) in common::corpus_files().iter().enumerate() {
        let name = common::corpus_name(input);
        let engine = qingpdf_core::view::Engine::start(Box::new(|| {}));
        engine.open_file(1, &input.to_string_lossy(), "", qingpdf_core::view::TOTAL_BYTES, 1920 * 1080);
        let opened = match engine.wait(60_000) {
            Some(Event::Opened(o)) => o,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let mut v = Viewer { engine, opened, next: 100 };
        if !v.opened.rights.annotate || v.opened.pages.is_empty() {
            skipped += 1;
            continue;
        }
        let size = v.opened.pages[0];
        let (w, h) = (size.width as f32, size.height as f32);
        let r = v.shape(0, ShapeKind::Rectangle, [0.1 * w, 0.1 * h], [0.6 * w, 0.5 * h], [0, 0, 255], 2.0, false);
        if r.status != EditStatus::Done {
            failures.push(format!("{name}: shape: {:?} {}", r.status, r.message));
            continue;
        }
        let r = v.shape(0, ShapeKind::Arrow, [0.1 * w, 0.6 * h], [0.9 * w, 0.9 * h], [255, 0, 0], 3.0, false);
        if r.status != EditStatus::Done {
            failures.push(format!("{name}: arrow: {:?} {}", r.status, r.message));
            continue;
        }
        // Every annotation of the first page that is not locked is moved, and the first one that can be resized and recoloured is.
        let list = v.annotations(0);
        let mut tried = 0usize;
        let mut bad = None;
        for a in list.iter().filter(|a| !a.locked()).take(6) {
            let r = v.mv(0, a, [4.0, 4.0]);
            if r.status != EditStatus::Done {
                // (An annotation written inside the page's own list cannot be changed; anything else is a failure.)
                if !r.message.contains("inside the page's own list") && !r.message.contains("locked") && !on_purpose(&r.message) {
                    bad = Some(format!("move of {} {}: {:?} {}", a.subtype, a.index, r.status, r.message));
                }
                continue;
            }
            tried += 1;
            if a.restylable() {
                let r = v.restyle(0, a, Some([0, 128, 0]), None, None);
                if r.status != EditStatus::Done && !on_purpose(&r.message) {
                    bad = Some(format!("recolour of {} {}: {:?} {}", a.subtype, a.index, r.status, r.message));
                }
            }
            if a.resizable() {
                let r = v.resize(0, a, [a.rect[0], a.rect[1], a.rect[2] + 10.0, a.rect[3] + 10.0]);
                if r.status != EditStatus::Done && !on_purpose(&r.message) {
                    bad = Some(format!("resize of {} {}: {:?} {}", a.subtype, a.index, r.status, r.message));
                }
            }
        }
        if let Some(why) = bad {
            failures.push(format!("{name}: {why}"));
            continue;
        }
        touched += tried;
        let saved = out.join(format!("{n:03}.pdf"));
        let (status, message, _) = v.save(&saved);
        if status != SaveStatus::Done {
            failures.push(format!("{name}: save: {message}"));
            continue;
        }
        let was_rewritten = r.rewrote;
        if !was_rewritten && !std::fs::read(&saved).unwrap().starts_with(&std::fs::read(input).unwrap()) {
            failures.push(format!("{name}: the bytes of the original changed"));
            continue;
        }
        let mut again = Viewer::open(&saved, "");
        if again.annotations(0).len() < list.len() {
            failures.push(format!("{name}: the reopened file lists {} annotations, not {}", again.annotations(0).len(), list.len()));
            continue;
        }
        if let Some(qpdf) = &qpdf {
            match common::judge_qpdf_output(qpdf, &saved, &[input.as_path()], false) {
                common::Judgement::Clean => {}
                common::Judgement::InputQuirk => quirks += 1,
                common::Judgement::Fail(why) => {
                    failures.push(format!("{name}: qpdf: {why}"));
                    continue;
                }
            }
        }
        done += 1;
    }
    println!("{done} files edited and saved ({quirks} with only the input's own qpdf warnings), {touched} existing annotations moved, {skipped} skipped");
    for f in &failures {
        println!("FAIL: {f}");
    }
    assert!(failures.is_empty(), "{} file(s) failed", failures.len());
    assert!(done > 50, "only {done} files were edited");
}
