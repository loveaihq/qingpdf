//! Page numbers, headers and footers (the first item of the list of features), through the engine's messages (the reader) and through
//! `edit::add_content` (the command line). Every saved file is checked as in `content.rs`: the original bytes a prefix, `qpdf --check`,
//! reopened. The pages are drawn by our own renderer and the text looked for where it must be (upright, at the edges of the page as it
//! is shown) on pages that are turned, whose boxes do not start at 0, whose resources are inherited or shared and whose content is one
//! stream, an array, shared or missing. The numbers themselves are read back out of the streams (through the font's `ToUnicode`). The
//! other readers are checked by `tests/tools/pagenum_check.py`, which draws the files these tests leave in the folder named by
//! `QINGPDF_PAGENUM_OUT` (default `tests/out/pagenum`).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;
mod package;

use common::Viewer;
use package::unzip;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use qingpdf_base::testutil::PdfBuilder;
use qingpdf_core::edit::{self, ContentOp, Pagination};
use qingpdf_core::object::{Object, ObjRef};
use qingpdf_core::text::TextExtractor;
use qingpdf_core::view::{ALL_PAGES, EditResult, EditStatus, Event, RenderRequest, Rendered, SaveStatus};
use qingpdf_core::Document;

impl Viewer {
    fn render(&mut self, page: u32) -> Rendered {
        self.render_at(page, 72.0)
    }

    fn render_at(&mut self, page: u32, dpi: f64) -> Rendered {
        let id = self.id();
        self.engine.render(RenderRequest { doc: 1, id, page, dpi, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
        self.until(|e| match e {
            Event::Rendered(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn number(&mut self, spec: Pagination) -> EditResult {
        let id = self.id();
        self.engine.add_pagination(1, id, spec);
        self.edited(id)
    }

    fn unnumber(&mut self) -> EditResult {
        let id = self.id();
        self.engine.remove_pagination(1, id);
        self.edited(id)
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
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
    let dir = std::env::var_os("QINGPDF_PAGENUM_OUT").map(PathBuf::from).unwrap_or_else(|| common::workspace_root().join("tests").join("out").join("pagenum"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn has_sans_font() -> bool {
    ["arial.ttf", "msyh.ttc", "simsun.ttc"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file())
}

fn has_chinese_font() -> bool {
    ["msyh.ttc", "simsun.ttc", "simhei.ttf"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file())
}

/// The slots, from head left to foot right.
fn slots(texts: [&str; 6]) -> [String; 6] {
    texts.map(ToString::to_string)
}

fn spec(texts: [&str; 6], pages: Vec<u32>) -> Pagination {
    Pagination { slots: slots(texts), size: 12.0, color: [255, 0, 0], margin: 10.0, pages, start: 1, mirror: false, skip_first: false, file: "shapes.pdf".to_string(), date: "2026-10-15".to_string() }
}

/// Five pages of every shape a page can have: P1 plain 300 x 400, one stream (shared with P4); P2 turned 90 degrees, resources that are an
/// object (shared with P3), content in an array; P3 boxes that do not start at 0 (a crop box inside), turned 270 degrees, no content at all;
/// P4 turned 180 degrees, inherits its resources (shared with P5) and shares P1's stream; P5 has an array object for content.
fn shapes() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 5 0 R 8 0 R 9 0 R 10 0 R] /Count 5 /Resources 21 0 R /MediaBox [0 0 300 400] >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /Helv 30 0 R >> >> /Contents 4 0 R >>");
    b.stream_obj(4, "", b"0 g 130 180 40 40 re f");
    b.obj(5, "<< /Type /Page /Parent 2 0 R /Rotate 90 /Resources 20 0 R /Contents [6 0 R 7 0 R] >>");
    b.stream_obj(6, "", b"BT /Helv 12 Tf 100 20 Td (hello) Tj ET");
    b.stream_obj(7, "", b"0 g 130 180 40 40 re f");
    b.obj(8, "<< /Type /Page /Parent 2 0 R /MediaBox [100 100 400 500] /CropBox [120 120 380 460] /Rotate 270 /Resources 20 0 R >>");
    b.obj(9, "<< /Type /Page /Parent 2 0 R /Rotate 180 /Contents 4 0 R >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /Contents 11 0 R >>");
    b.obj(11, "[4 0 R 6 0 R]");
    b.obj(20, "<< /Font << /Helv 30 0 R >> /ProcSet [/PDF /Text] >>");
    b.obj(21, "<< /Font << /Helv 30 0 R >> >>");
    b.obj(30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.finish_classic(40, "/Root 1 0 R")
}

/// `n` pages of 300 x 400 with a line of text each.
fn book(n: usize) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..n).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {n} /MediaBox [0 0 300 400] /Resources << /Font << /F1 5 0 R >> >> >>"));
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for i in 0..n {
        b.obj(10 + 2 * i as u32, &format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i));
        b.stream_obj(11 + 2 * i as u32, "", format!("BT /F1 12 Tf 40 300 Td (Body of page {}) Tj ET", i + 1).as_bytes());
    }
    b.finish_classic(10 + 2 * n as u32, "/Root 1 0 R")
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

fn dict_of(doc: &Document, r: ObjRef) -> qingpdf_core::Dict {
    match doc.get(r).unwrap() {
        Object::Dict(d) => d,
        other => panic!("{r:?} is not a dictionary: {other:?}"),
    }
}

fn stream_bytes(doc: &Document, r: ObjRef) -> Vec<u8> {
    let Object::Stream(s) = doc.get(r).unwrap() else { panic!("{r:?} is not a stream") };
    doc.decode_stream(&s).unwrap()
}

/// The character each code of a font stands for, from its `/ToUnicode` (`beginbfchar` entries).
fn code_map(doc: &Document, font: ObjRef) -> HashMap<u16, char> {
    let to_unicode = dict_of(doc, font).get("ToUnicode").and_then(Object::as_obj_ref).expect("a ToUnicode CMap");
    let text = String::from_utf8(stream_bytes(doc, to_unicode)).unwrap();
    let mut map = HashMap::new();
    let mut in_chars = false;
    for line in text.lines() {
        if line.ends_with("beginbfchar") {
            in_chars = true;
        } else if line.starts_with("endbfchar") {
            in_chars = false;
        } else if in_chars {
            let hex: Vec<&str> = line.split(['<', '>']).filter(|s| !s.trim().is_empty()).collect();
            let (code, to) = (u16::from_str_radix(hex[0], 16).unwrap(), u32::from_str_radix(hex[1], 16).unwrap());
            map.insert(code, char::from_u32(to).unwrap());
        }
    }
    map
}

/// What the page (from 0) shows of what was put on it: the streams listed under its private key, in the order they are drawn, with the
/// text of each `Tj` read through the font: (the kind of the sequence it is in, the text).
fn written(doc: &Document, page: usize) -> Vec<(String, String)> {
    let p = &doc.pages().unwrap()[page];
    let dict = dict_of(doc, p.obj_ref);
    let Some(Object::Array(ours)) = dict.get("QpPagination") else { return Vec::new() };
    let resources = match dict.get("Resources") {
        Some(Object::Dict(d)) => d.clone(),
        Some(other) => doc.resolve(other).unwrap().as_dict().unwrap().clone(),
        None => panic!("page {page} has no resources"),
    };
    let fonts = match resources.get("Font") {
        Some(Object::Dict(d)) => d.clone(),
        Some(other) => doc.resolve(other).unwrap().as_dict().unwrap().clone(),
        None => panic!("page {page}: no fonts"),
    };
    let (_, font) = fonts.iter().find(|(k, _)| k.as_bytes().starts_with(b"Qn")).expect("the font of the numbers in the resources");
    let codes = code_map(doc, font.as_obj_ref().unwrap());
    let mut out = Vec::new();
    for r in ours {
        let text = String::from_utf8(stream_bytes(doc, r.as_obj_ref().unwrap())).unwrap();
        let mut kind = String::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("/Artifact <</Type /Pagination /Subtype /") {
                kind = rest.split(">>").next().unwrap().to_string();
            } else if let Some(hex) = line.strip_suffix(" Tj").and_then(|l| l.rsplit('<').next()) {
                let hex = hex.trim_end_matches('>');
                let shown: String = hex.as_bytes().chunks(4).map(|c| codes[&u16::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap()]).collect();
                out.push((kind.clone(), shown));
            }
        }
    }
    out
}

/// The box (left, top, right, bottom, inclusive) of the pixels that `test` accepts within the window, and how many pixels of its top and
/// of its bottom row there are.
fn ink_in(b: &Rendered, window: [usize; 4], test: impl Fn([u8; 3]) -> bool) -> Option<([usize; 4], usize, usize)> {
    let w = b.width as usize;
    let mut found: Option<[usize; 4]> = None;
    let hit = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        test([b.bgra[i + 2], b.bgra[i + 1], b.bgra[i]])
    };
    for y in window[1]..window[3].min(b.height as usize) {
        for x in window[0]..window[2].min(w) {
            if hit(x, y) {
                found = Some(match found {
                    Some([l, t, r, bt]) => [l.min(x), t.min(y), r.max(x), bt.max(y)],
                    None => [x, y, x, y],
                });
            }
        }
    }
    let f = found?;
    let row = |y: usize| (f[0]..=f[2]).filter(|&x| hit(x, y)).count();
    Some((f, row(f[1]), row(f[3])))
}

/// Reddish: what the text of the test is made of, edges and all (the page is white and black otherwise).
fn red(c: [u8; 3]) -> bool {
    i32::from(c[0]) - i32::from(c[1]) > 40
}

#[test]
fn the_six_places_are_upright_and_at_the_edges_of_the_page_as_it_is_shown_whatever_its_turn_and_box() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let out = out_dir();
    let input = out.join("shapes.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let doc = Document::open(&input).unwrap();
    // `LLL` is not the same upside down (its foot is a full row) nor sideways (it is wider than high).
    let op = ContentOp::Pagination(spec(["LLL"; 6], (0..5).collect()));
    let added = edit::add_content(&doc, "", &op).unwrap();
    assert!(!added.rewritten);
    let saved = out.join("shapes-numbered.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    check_saved(&input, &saved, "");

    // Drawn at 144 dpi, two pixels to the point.
    let mut v = Viewer::open(&saved, "");
    let (k, size) = (2.0, 12.0 * 2.0);
    let margin = 10.0 * 72.0 / 25.4 * k;
    for p in 0..5u32 {
        let b = v.render_at(p, 144.0);
        let (w, h) = (b.width as usize, b.height as usize);
        // The page as it is shown: 300 x 400, 400 x 300, the crop box turned (340 x 260), 300 x 400, 300 x 400 points.
        assert_eq!((w, h), [(600, 800), (800, 600), (680, 520), (600, 800), (600, 800)][p as usize], "page {p}");
        let thirds = [[0, w / 3], [w / 3, 2 * w / 3], [2 * w / 3, w]];
        for (k, side) in [0usize, 1, 2, 0, 1, 2].into_iter().enumerate() {
            let head = k < 3;
            let window = if head { [thirds[side][0], 0, thirds[side][1], h / 4] } else { [thirds[side][0], 3 * h / 4, thirds[side][1], h] };
            let (bx, top_row, bottom_row) = ink_in(&b, window, red).unwrap_or_else(|| panic!("page {p}: nothing in place {k}"));
            let (left, right, top, bottom) = (bx[0] as f64, bx[2] as f64 + 1.0, bx[1] as f64, bx[3] as f64 + 1.0);
            // Upright: wider than high, and the foot of the L is the full bottom row.
            assert!(right - left > 1.5 * (bottom - top), "page {p}, place {k}: box {bx:?} is not a line of text lying flat");
            assert!(bottom_row > 2 * top_row, "page {p}, place {k}: the letters are upside down ({top_row} ink pixels on top, {bottom_row} at the bottom)");
            // The edge: the line is `margin` from the top (the head) or the bottom (the foot); the letters stand on the baseline, a
            // little above the foot of the line, and hang a little below its top.
            if head {
                assert!(top >= margin - 1.0 && top <= margin + size, "page {p}, place {k}: the top of the letters is at {top}, the margin is {margin}");
            } else {
                assert!(bottom <= h as f64 - margin + 1.0 && bottom >= h as f64 - margin - 0.4 * size, "page {p}, place {k}: the foot of the letters is at {bottom}, the margin is {margin} from {h}");
            }
            match side {
                0 => assert!(left >= margin - 2.0 && left <= margin + 8.0, "page {p}, place {k}: left edge {left}"),
                1 => assert!(((left + right) / 2.0 - w as f64 / 2.0).abs() <= 4.0, "page {p}, place {k}: the middle is at {}", (left + right) / 2.0),
                _ => assert!(right <= w as f64 - margin + 2.0 && right >= w as f64 - margin - 10.0, "page {p}, place {k}: right edge {right}"),
            }
        }
    }
}

#[test]
fn the_numbers_follow_the_range_the_start_the_skipped_first_page_and_the_sides_change_on_the_even_pages() {
    let out = out_dir();
    let input = out.join("book.pdf");
    std::fs::write(&input, book(6)).unwrap();
    let doc = Document::open(&input).unwrap();
    let make = |name: &str, spec: Pagination| {
        let added = edit::add_content(&doc, "", &ContentOp::Pagination(spec)).unwrap();
        let path = out.join(name);
        std::fs::write(&path, &added.data).unwrap();
        check_saved(&input, &path, "");
        Document::open(&path).unwrap()
    };
    let lines = |d: &Document, page: usize| -> Vec<String> { written(d, page).into_iter().map(|(_, t)| t).collect() };
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    // The foot in the middle, the file's name and the date at the head, the total: every field, on every page.
    let all = make("book-all.pdf", Pagination { file: "annual-report.pdf".to_string(), ..spec(["{file}", "{date}", "", "", "Page {page} of {pages}", ""], (0..6).collect()) });
    for p in 0..6 {
        assert_eq!(lines(&all, p), ["annual-report.pdf".to_string(), "2026-10-15".to_string(), format!("Page {} of 6", p + 1)], "page {p}");
    }
    // Chinese: the gongwen way (page 3 of 6).
    if has_chinese_font() {
        let zh = make("book-zh.pdf", spec(["", "", "", "", "- {page} -", "第 {page} 页 共 {pages} 页"], (0..6).collect()));
        assert_eq!(lines(&zh, 2), ["- 3 -", "第 3 页 共 6 页"]);
    }
    // From the third page, the first of them reading 1, nothing on that first page: 4 of 4 on the last.
    let from_three = make("book-from-3.pdf", Pagination { start: 1, skip_first: true, ..spec(["", "", "", "", "{page} / {pages}", ""], (2..6).collect()) });
    assert_eq!(lines(&from_three, 0), Vec::<String>::new());
    assert_eq!(lines(&from_three, 1), Vec::<String>::new());
    assert_eq!(lines(&from_three, 2), Vec::<String>::new(), "the first page of the run is left out (and counted: the next one is 2)");
    assert_eq!(lines(&from_three, 3), vec!["2 / 4"]);
    assert_eq!(lines(&from_three, 5), vec!["4 / 4"]);
    // Start at 0 and at 100; the left and right places change sides on the second, fourth, sixth page.
    let start = make("book-start.pdf", Pagination { start: 100, mirror: true, ..spec(["", "", "", "L{page}", "", "R{page}"], (0..6).collect()) });
    assert_eq!(lines(&start, 0), vec!["L100", "R100"]);
    assert_eq!(lines(&start, 1), vec!["R101", "L101"], "a left-hand page has the places the other way round");
    assert_eq!(lines(&start, 5), vec!["R105", "L105"]);
    assert_eq!(lines(&make("book-zero.pdf", Pagination { start: 0, ..spec(["{page}", "", "", "", "", ""], vec![3]) }), 3), vec!["0"]);
    // The marks of the head are Header and those of the foot Footer.
    let kinds: Vec<String> = written(&all, 0).into_iter().map(|(k, _)| k).collect();
    assert_eq!(kinds, ["Header", "Header", "Footer"]);
    // Pages 3 to 6 numbered from 1 without the first: page 3 has nothing, so the run's numbers are 2, 3, 4 on pages 4, 5, 6.
    assert_eq!(lines(&from_three, 4), vec!["3 / 4"]);
}

#[test]
fn the_sequence_is_an_artifact_the_text_is_not_changed_and_the_exports_leave_it_out() {
    let out = out_dir();
    let input = out.join("book-artifact.pdf");
    std::fs::write(&input, book(3)).unwrap();
    let doc = Document::open(&input).unwrap();
    let mut extractor = TextExtractor::new(&doc);
    let before: Vec<String> = doc.pages().unwrap().iter().map(|p| extractor.page_text(p).unwrap()).collect();
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let op = ContentOp::Pagination(spec(["ZQZQ head", "", "", "ZQZQ foot {page}", "", "ZQZQ {file}"], (0..3).collect()));
    let added = edit::add_content(&doc, "", &op).unwrap();
    let saved = out.join("book-artifact-numbered.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    check_saved(&input, &saved, "");
    let after = Document::open(&saved).unwrap();
    // Marked as artifacts of the page (14.8.2.2): a header and a footer, each in one sequence.
    let page = &after.pages().unwrap()[0];
    let Some(Object::Array(ours)) = dict_of(&after, page.obj_ref).get("QpPagination").cloned() else { panic!("no private key") };
    assert_eq!(ours.len(), 1);
    let drawing = String::from_utf8(stream_bytes(&after, ours[0].as_obj_ref().unwrap())).unwrap();
    assert!(drawing.contains("/Artifact <</Type /Pagination /Subtype /Header>> BDC") && drawing.contains("/Artifact <</Type /Pagination /Subtype /Footer>> BDC"), "{drawing}");
    assert_eq!(drawing.matches("BDC").count(), 2);
    assert_eq!(drawing.matches("EMC").count(), 2);
    // The text of the pages is what it was: our extractor leaves the artifacts out (and so does everything built on it).
    let mut extractor = TextExtractor::new(&after);
    for (i, p) in after.pages().unwrap().iter().enumerate() {
        let text = extractor.page_text(p).unwrap();
        assert_eq!(text, before[i], "page {i}");
        assert!(!text.contains("ZQZQ"));
    }
    // Word and Excel: nothing of it in any part.
    let mut docx = Vec::new();
    qingpdf_core::docx::export(&after, "", None, &mut docx, None).unwrap();
    let mut xlsx = Vec::new();
    qingpdf_core::xlsx::export(&after, "", None, &mut xlsx, None).unwrap();
    for (what, bytes) in [("docx", docx), ("xlsx", xlsx)] {
        let parts = unzip(&bytes);
        assert!(!parts.is_empty());
        for (name, data) in parts {
            assert!(!String::from_utf8_lossy(&data).contains("ZQZQ"), "{what}: {name} has the page numbers' text");
        }
    }
}

#[test]
fn a_second_run_adds_another_set_and_taking_them_away_leaves_the_page_as_it_was_and_undo_puts_them_back() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let out = out_dir();
    let input = out.join("shapes-undo.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let mut v = Viewer::open(&input, "");
    let plain: Vec<Rendered> = (0..5).map(|p| v.render(p)).collect();
    let r = v.unnumber();
    assert_eq!((r.status, r.dirty), (EditStatus::Failed, false), "nothing of ours to take away: {r:?}");
    let r = v.number(spec(["", "", "", "", "{page}", ""], (0..5).collect()));
    assert_eq!((r.status, r.page, r.can_undo, r.dirty), (EditStatus::Done, ALL_PAGES, true, true), "{r:?}");
    let numbered: Vec<Rendered> = (0..5).map(|p| v.render(p)).collect();
    assert!((0..5).all(|p| numbered[p].bgra != plain[p].bgra));
    // A second set on top of the first (the same place, so the same pixels; one more set of streams).
    let r = v.number(spec(["", "", "", "", "{page}", ""], vec![0, 1]));
    assert_eq!(r.status, EditStatus::Done, "{r:?}");
    let saved = out.join("shapes-two-sets.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&input, &saved, "");
    let two = Document::open(&saved).unwrap();
    let sets = |d: &Document, page: usize| match dict_of(d, d.pages().unwrap()[page].obj_ref).get("QpPagination") {
        Some(Object::Array(a)) => a.len(),
        _ => 0,
    };
    assert_eq!((sets(&two, 0), sets(&two, 1), sets(&two, 2)), (2, 2, 1));
    // Taking them away: every page as it was, however many sets it had; the page's own content is not touched.
    let r = v.unnumber();
    assert_eq!((r.status, r.page), (EditStatus::Done, ALL_PAGES), "{r:?}");
    for p in 0..5u32 {
        assert_eq!(v.render(p).bgra, plain[p as usize].bgra, "page {p} is not as it was");
    }
    let gone = out.join("shapes-unnumbered.pdf");
    assert_eq!(v.save(&gone).0, SaveStatus::Done);
    check_saved(&saved, &gone, "");
    let after = Document::open(&gone).unwrap();
    assert_eq!((sets(&after, 0), sets(&after, 1)), (0, 0));
    // The streams of the page: the original one is still in its place, with only the little balancing streams round it.
    let list = |d: &Document, page: usize| match dict_of(d, d.pages().unwrap()[page].obj_ref).get("Contents") {
        Some(Object::Array(a)) => a.iter().map(|o| o.as_obj_ref().unwrap().num).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert!(list(&after, 0).contains(&4), "the page's own stream is listed");
    assert_eq!(stream_bytes(&after, ObjRef::new(4, 0)), b"0 g 130 180 40 40 re f");
    // Undoing the taking away brings both sets back: the pages that had one set are as they were with it.
    let r = v.undo();
    assert_eq!((r.status, r.page), (EditStatus::Done, ALL_PAGES));
    for p in 0..5u32 {
        let again = v.render(p);
        assert_ne!(again.bgra, plain[p as usize].bgra, "page {p}: the numbers are back");
        if p >= 2 {
            assert_eq!(again.bgra, numbered[p as usize].bgra, "page {p}");
        }
    }
    // A file that was not numbered by us has nothing to take away, whatever it holds.
    let mut other = Viewer::open(&input, "");
    assert_eq!(other.unnumber().status, EditStatus::Failed);
}

#[test]
fn rights_signatures_limits_and_bad_input_are_what_the_other_content_edits_say() {
    let out = out_dir();
    let input = out.join("shapes-rights.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let doc = Document::open(&input).unwrap();
    let good = spec(["", "", "", "", "{page}", ""], (0..5).collect());
    let fails = |op: &Pagination, what: &str| {
        let err = edit::add_content(&doc, "", &ContentOp::Pagination(op.clone())).expect_err(what).to_string();
        assert!(!err.is_empty());
        err
    };
    let mut v = Viewer::open(&input, "");
    let failed = |r: EditResult, what: &str| {
        assert_eq!(r.status, EditStatus::Failed, "{what}: {r:?}");
        assert!(!r.dirty, "{what}");
    };
    // Nothing to write, no page, a page that is not there, a template of 201 characters, a first number that is too big, a margin that is
    // not one, a size that is not a number.
    assert!(fails(&spec([" ", "", "", "", "", ""], vec![0]), "blank").contains("nothing"));
    assert!(fails(&Pagination { pages: Vec::new(), ..good.clone() }, "no pages").contains("no pages"));
    assert!(fails(&Pagination { pages: vec![9], ..good.clone() }, "page 9").contains("no such page"));
    assert!(fails(&spec([&"x".repeat(201), "", "", "", "", ""], vec![0]), "201").contains("200"));
    assert!(fails(&Pagination { start: edit::MAX_START + 1, ..good.clone() }, "start").contains("1000000"));
    assert!(fails(&Pagination { margin: f32::NAN, ..good.clone() }, "margin").contains("margin"));
    assert!(fails(&Pagination { margin: 501.0, ..good.clone() }, "margin").contains("margin"));
    assert!(fails(&Pagination { size: f32::NAN, ..good.clone() }, "size").contains("size"));
    assert!(fails(&Pagination { pages: vec![0], skip_first: true, ..good.clone() }, "only page skipped").contains("left out"));
    failed(v.number(spec([" ", "", "", "", "", ""], vec![0])), "blank");
    failed(v.number(Pagination { pages: vec![0; edit::MAX_PAGINATION_PAGES + 1], ..good.clone() }), "too many pages");
    failed(v.number(Pagination { pages: vec![9], ..good.clone() }), "page 9");
    // A file that does not allow changing its content (permission bit 4): refused in the reader and on the command line; the owner's
    // password lifts it.
    if let Some(qpdf) = common::find_qpdf() {
        let locked = out.join("shapes-nomodify.pdf");
        let status = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner", "256", "--modify=none", "--annotate=y", "--", &input.to_string_lossy(), &locked.to_string_lossy()]).status().unwrap();
        assert!(status.code().is_some_and(|c| c == 0 || c == 3));
        let mut locked_view = Viewer::open(&locked, "");
        assert_eq!(locked_view.number(good.clone()).status, EditStatus::Denied);
        assert_eq!(locked_view.unnumber().status, EditStatus::Denied);
        let locked_doc = Document::open_with_password(&locked, "").unwrap();
        let err = edit::add_content(&locked_doc, "", &ContentOp::Pagination(good.clone())).unwrap_err().to_string();
        assert!(err.contains("permission bit 4") && err.contains("owner password"), "{err}");
        if has_sans_font() {
            let owner_doc = Document::open_with_password(&locked, "owner").unwrap();
            assert!(edit::add_content(&owner_doc, "owner", &ContentOp::Pagination(good.clone())).is_ok());
        }
    }
    // A signed file, and a certified one: refused (and the file is not touched).
    let signed = {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] /SigFlags 3 >> >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Annots [10 0 R] >>");
        b.obj(10, "<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V 20 0 R >>");
        b.obj(20, "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>");
        b.finish_classic(30, "/Root 1 0 R")
    };
    let path = out.join("signed.pdf");
    std::fs::write(&path, &signed).unwrap();
    let mut sv = Viewer::open(&path, "");
    let r = sv.number(spec(["", "", "", "", "{page}", ""], vec![0]));
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    assert!(r.message.contains("signature") || r.message.contains("signed"), "{}", r.message);
    assert_eq!(sv.unnumber().status, EditStatus::Failed);
    let signed_doc = Document::open(&path).unwrap();
    let err = edit::add_content(&signed_doc, "", &ContentOp::Pagination(spec(["", "", "", "", "{page}", ""], vec![0]))).unwrap_err().to_string();
    assert!(err.contains("signed") || err.contains("certified"), "{err}");
    assert_eq!(sv.save(&out.join("signed-saved.pdf")).0, SaveStatus::Done);
    assert_eq!(std::fs::read(out.join("signed-saved.pdf")).unwrap(), signed, "nothing was changed, so nothing is written");
}

#[test]
fn the_numbers_of_an_encrypted_file_are_the_same_and_the_file_keeps_its_encryption() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: no qpdf");
        return;
    };
    let out = out_dir();
    let plain = out.join("book-for-aes.pdf");
    std::fs::write(&plain, book(4)).unwrap();
    for (bits, name) in [("256", "aes256"), ("128", "aes128")] {
        let locked = out.join(format!("book-{name}.pdf"));
        let aes = if bits == "128" { "--use-aes=y" } else { "--" };
        let status = std::process::Command::new(&qpdf).args(["--encrypt", "pw", "pw", bits, aes, "--", &plain.to_string_lossy(), &locked.to_string_lossy()]).status().unwrap();
        assert!(status.code().is_some_and(|c| c == 0 || c == 3));
        let doc = Document::open_with_password(&locked, "pw").unwrap();
        let added = edit::add_content(&doc, "pw", &ContentOp::Pagination(spec(["", "", "", "", "{page} of {pages}", ""], (0..4).collect()))).unwrap();
        let saved = out.join(format!("book-{name}-numbered.pdf"));
        std::fs::write(&saved, &added.data).unwrap();
        check_saved(&locked, &saved, "pw");
        let after = Document::open_with_password(&saved, "pw").unwrap();
        assert!(after.is_encrypted());
        assert_eq!(written(&after, 2), vec![("Footer".to_string(), "3 of 4".to_string())], "{name}");
        // The reader saves the same.
        let mut v = Viewer::open(&locked, "pw");
        assert_eq!(v.number(spec(["", "", "", "", "{page} of {pages}", ""], (0..4).collect())).status, EditStatus::Done);
        let by_reader = out.join(format!("book-{name}-reader.pdf"));
        assert_eq!(v.save(&by_reader).0, SaveStatus::Done);
        check_saved(&locked, &by_reader, "pw");
    }
}

#[test]
fn five_hundred_pages_take_well_under_two_seconds_and_share_one_font() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let out = out_dir();
    let input = out.join("book-500.pdf");
    std::fs::write(&input, book(500)).unwrap();
    let doc = Document::open(&input).unwrap();
    let started = std::time::Instant::now();
    let op = ContentOp::Pagination(spec(["{file}", "", "{date}", "", "第 {page} 页 共 {pages} 页", ""], (0..500).collect()));
    let op = if has_chinese_font() { op } else { ContentOp::Pagination(spec(["{file}", "", "{date}", "", "{page} / {pages}", ""], (0..500).collect())) };
    let added = edit::add_content(&doc, "", &op).unwrap();
    let took = started.elapsed();
    if !cfg!(debug_assertions) {
        assert!(took.as_secs_f64() < 2.0, "it took {took:?}");
    }
    let saved = out.join("book-500-numbered.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    check_saved(&input, &saved, "");
    // One font program for all the pages: the update has one `FontFile2` and 500 drawings.
    let update = &added.data[std::fs::metadata(&input).unwrap().len() as usize..];
    let count = |needle: &[u8]| update.windows(needle.len()).filter(|w| *w == needle).count();
    assert_eq!(count(b"/FontFile2"), 1, "one subset for all the pages");
    assert_eq!(count(b"/QpPagination"), 500);
    println!("500 pages numbered in {took:?}, {} bytes appended", update.len());
}

#[test]
fn a_template_with_only_the_number_of_pages_has_every_digit_in_the_font() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let out = out_dir();
    let input = out.join("book-12.pdf");
    std::fs::write(&input, book(12)).unwrap();
    let doc = Document::open(&input).unwrap();
    // 12 pages and no `{page}`: the font must have the 1 and the 2, not only the 0 (the text read back through the font's own ToUnicode
    // would otherwise be `of 00`, or not be found at all).
    let op = ContentOp::Pagination(spec(["", "", "", "", "of {pages}", ""], (0..12).collect()));
    let added = edit::add_content(&doc, "", &op).unwrap();
    let saved = out.join("book-12-of-pages.pdf");
    std::fs::write(&saved, &added.data).unwrap();
    check_saved(&input, &saved, "");
    let after = Document::open(&saved).unwrap();
    for page in [0, 5, 11] {
        assert_eq!(written(&after, page), vec![("Footer".to_string(), "of 12".to_string())], "page {page}");
    }
}

#[test]
fn a_forged_key_cannot_make_the_removal_take_out_a_stream_that_is_the_pages_own() {
    let out = out_dir();
    // Page 1 says its body stream (4) is ours; page 2 lists a stream that is ours (7, what the engine writes) and one that is its body (6)
    // in the key; page 3 has a key that lists a stream that is not in its content.
    let ours = "q 1 0 0 1 0 0 cm\n/Artifact <</Type /Pagination /Subtype /Footer>> BDC\nBT\nET\nEMC\nQ\n";
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 5 0 R 8 0 R] /Count 3 /MediaBox [0 0 300 400] >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /Contents [4 0 R] /QpPagination [4 0 R] >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 40 300 Td (Body of page one) Tj ET");
    b.obj(5, "<< /Type /Page /Parent 2 0 R /Contents [6 0 R 7 0 R] /QpPagination [6 0 R 7 0 R 99 0 R] >>");
    b.stream_obj(6, "", b"BT /F1 12 Tf 40 300 Td (Body of page two) Tj ET");
    b.stream_obj(7, "", ours.as_bytes());
    b.obj(8, "<< /Type /Page /Parent 2 0 R /Contents [9 0 R] /QpPagination [7 0 R] >>");
    b.stream_obj(9, "", b"BT /F1 12 Tf 40 300 Td (Body of page three) Tj ET");
    let input = out.join("forged.pdf");
    std::fs::write(&input, b.finish_classic(20, "/Root 1 0 R")).unwrap();
    let doc = Document::open(&input).unwrap();
    let pages = doc.pages().unwrap();
    let changed = edit::apply(&doc, &pages, &edit::Edit::Unpaginate, "").unwrap();
    // Only page 2 is changed, and it loses only the stream that is ours.
    assert_eq!(changed.len(), 1, "{changed:?}");
    let (num, Object::Dict(page)) = &changed[0] else { panic!("{changed:?}") };
    assert_eq!(*num, 5);
    let Some(Object::Array(list)) = page.get("Contents") else { panic!("{page:?}") };
    assert_eq!(list.iter().filter_map(|o| o.as_obj_ref().map(|r| r.num)).collect::<Vec<_>>(), vec![6], "the body stays, ours goes");
    assert!(page.get("QpPagination").is_none());
    // A file with nothing but forged keys has nothing to take away.
    let mut v = Viewer::open(&input, "");
    let r = v.unnumber();
    assert_eq!(r.status, EditStatus::Done, "{r:?}");
    let forged_only = out.join("forged-only.pdf");
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /Contents [4 0 R] /QpPagination [4 0 R] >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 40 300 Td (Body) Tj ET");
    std::fs::write(&forged_only, b.finish_classic(10, "/Root 1 0 R")).unwrap();
    let mut w = Viewer::open(&forged_only, "");
    let r = w.unnumber();
    assert_eq!((r.status, r.dirty), (EditStatus::Failed, false), "{r:?}");
    assert!(r.message.contains("no page numbers"), "{}", r.message);
}
