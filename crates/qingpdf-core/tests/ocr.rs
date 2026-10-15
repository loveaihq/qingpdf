//! Step 5c: the engine's side of recognising scans: which pages are scans, the page drawn as grey pixels for the recogniser, and the
//! invisible text layer written from the words it finds (the recogniser itself is the window's: here the words are made up). The
//! saved files are checked as in `content.rs` (original bytes a prefix, `qpdf --check`, reopened), and the layer is looked for where
//! the words are on pages of every turn and box.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use qingpdf_core::Document;
use qingpdf_core::render::work::Work;
use qingpdf_core::text::TextExtractor;
use qingpdf_core::view::{EditResult, EditStatus, Engine, Event, OcrLine, OcrPage, OcrWord, Opened, RenderStatus, SaveStatus, TOTAL_BYTES, ALL_PAGES};

struct Viewer {
    engine: Engine,
    #[allow(dead_code)]
    opened: Opened,
    next: u64,
}

impl Viewer {
    fn open(path: &Path) -> Viewer {
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &path.to_string_lossy(), "", TOTAL_BYTES, 1920 * 1080);
        match engine.wait(20_000) {
            Some(Event::Opened(opened)) => Viewer { engine, opened, next: 100 },
            other => panic!("{}: not opened: {other:?}", path.display()),
        }
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn until<T>(&self, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        loop {
            match self.engine.wait(30_000) {
                Some(e) => {
                    if let Some(t) = pick(e) {
                        return t;
                    }
                }
                None => panic!("the engine said nothing for thirty seconds"),
            }
        }
    }

    /// (pages, looked at, truncated)
    fn scan(&mut self, only: Option<u32>) -> (Vec<u32>, u32, bool) {
        let id = self.id();
        self.engine.ocr_scan(1, id, only);
        self.until(|e| match e {
            Event::OcrPages { id: i, pages, looked, truncated, .. } if i == id => Some((pages, looked, truncated)),
            _ => None,
        })
    }

    /// (status, message, width, height, dpi, grey)
    fn image(&mut self, page: u32, max_side: u32) -> (RenderStatus, String, u32, u32, f64, Vec<u8>) {
        let id = self.id();
        self.engine.ocr_image(1, id, page, max_side);
        self.until(|e| match e {
            Event::OcrImage { id: i, status, message, width, height, dpi, gray, .. } if i == id => Some((status, message, width, height, dpi, gray)),
            _ => None,
        })
    }

    fn layer(&mut self, pages: Vec<OcrPage>) -> EditResult {
        let id = self.id();
        self.engine.ocr_layer(1, id, pages);
        self.until(|e| match e {
            Event::Edited(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
        self.until(|e| match e {
            Event::Edited(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn save(&mut self, path: &Path) -> (SaveStatus, String) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, .. } if i == id => Some((status, message)),
            _ => None,
        })
    }

    /// The characters of a page and their boxes as the reader gets them (text and boxes together, by copying the whole page).
    fn chars(&mut self, page: u32) -> (String, Vec<[f32; 4]>) {
        let id = self.id();
        self.engine.char_boxes(1, id, page);
        let boxes = self.until(|e| match e {
            Event::CharBoxes(b) if b.id == id => Some(b.boxes),
            _ => None,
        });
        let id = self.id();
        self.engine.copy_text(1, id, (page, 0), (page, boxes.len() as u32));
        let text = self.until(|e| match e {
            Event::Copied { id: i, text, .. } if i == id => Some(text),
            _ => None,
        });
        (text, boxes)
    }

    fn found(&mut self, query: &str) -> usize {
        let id = self.id();
        self.engine.search(1, id, query, 0);
        let mut hits = 0;
        self.until(|e| match e {
            Event::SearchHits { id: i, hits: h, .. } if i == id => {
                hits += h.len();
                None
            }
            Event::SearchDone { id: i, .. } if i == id => Some(()),
            _ => None,
        });
        hits
    }
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("QINGPDF_OCR_OUT").map(PathBuf::from).unwrap_or_else(|| common::workspace_root().join("tests").join("out").join("ocr"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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

/// A grey picture of 40 by 40 pixels, white with a black square in the middle, as an image object (in ASCII hex, so that it is text).
fn square_image() -> String {
    let mut hex = String::new();
    for y in 0..40 {
        for x in 0..40 {
            hex.push_str(if (15..25).contains(&x) && (15..25).contains(&y) { "00" } else { "FF" });
        }
    }
    hex.push('>');
    format!("<< /Type /XObject /Subtype /Image /Width 40 /Height 40 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}\nendstream", hex.len())
}

/// Pages: P1 a scan (a picture over the whole page, no text); P2 the same picture with a paragraph of text; P3 a small picture and
/// nothing else; P4 a scan with a page number as its only text; P5 text only.
fn mixed() -> Vec<u8> {
    let page = |content: u32| format!("<< /Type /Page /Parent 2 0 R /Resources << /XObject << /Im0 20 0 R >> /Font << /Helv 30 0 R >> >> /Contents {content} 0 R >>");
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 5 /MediaBox [0 0 300 400] >>".to_string()),
        (3, page(10)),
        (4, page(11)),
        (5, page(12)),
        (6, page(13)),
        (7, page(14)),
        (10, stream("q 300 0 0 400 0 0 cm /Im0 Do Q")),
        (11, stream("q 300 0 0 400 0 0 cm /Im0 Do Q BT /Helv 12 Tf 20 300 Td (A paragraph of real text that is on the page) Tj ET")),
        (12, stream("q 60 0 0 60 20 20 cm /Im0 Do Q")),
        (13, stream("q 300 0 0 400 0 0 cm /Im0 Do Q BT /Helv 10 Tf 140 10 Td (12) Tj ET")),
        (14, stream("BT /Helv 12 Tf 20 300 Td (Only text here, no picture at all) Tj ET")),
        (20, square_image()),
        (30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string()),
    ];
    pdf(&objects, "")
}

fn words(line: &[(&str, [f32; 4])]) -> OcrLine {
    OcrLine { words: line.iter().map(|(t, r)| OcrWord { text: (*t).to_string(), rect: *r }).collect() }
}

#[test]
fn scans_are_the_pages_with_no_text_that_are_mostly_pictures() {
    let dir = out_dir();
    let file = dir.join("mixed.pdf");
    std::fs::write(&file, mixed()).unwrap();
    let mut v = Viewer::open(&file);
    let (pages, looked, truncated) = v.scan(None);
    assert_eq!((pages, looked, truncated), (vec![0, 3], 5, false), "the scan and the scan with a page number only");
    // A page asked for by itself needs only to have no text (the small picture counts), not to be mostly a picture.
    assert_eq!(v.scan(Some(2)).0, vec![2]);
    assert_eq!(v.scan(Some(1)).0, Vec::<u32>::new(), "a page with text is not recognised");
    assert_eq!(v.scan(Some(4)).0, Vec::<u32>::new());
    assert_eq!(v.scan(Some(99)).0, Vec::<u32>::new(), "no such page: nothing, and no panic");
}

#[test]
fn a_page_is_drawn_as_grey_pixels_at_300_dpi_or_less_for_what_the_recogniser_takes() {
    let dir = out_dir();
    let file = dir.join("mixed-image.pdf");
    std::fs::write(&file, mixed()).unwrap();
    let mut v = Viewer::open(&file);
    // 300 x 400 points at 300 dpi.
    let (status, message, w, h, dpi, gray) = v.image(0, 10_000);
    assert_eq!((status, w, h), (RenderStatus::Complete, 1250, 1667), "{message}");
    assert!((dpi - 300.0).abs() < 1e-9);
    assert_eq!(gray.len(), w as usize * h as usize);
    // The square is in the middle (15 to 25 of 40 pixels of the picture, which covers the page): black in the middle, white at the edge.
    let at = |x: usize, y: usize| gray[y * w as usize + x];
    assert!(at(625, 833) < 16 && at(10, 10) > 240, "{} {}", at(625, 833), at(10, 10));
    // A recogniser that takes 1000 pixels on a side gets a smaller picture.
    let (status, _, w, h, dpi, _) = v.image(0, 1_000);
    assert_eq!(status, RenderStatus::Complete);
    assert!(w <= 1000 && h <= 1000 && h > 990 && dpi < 180.0, "{w} x {h} at {dpi}");
    // No such page, and a page too big for any useful resolution, fail with a reason and without a picture.
    let (status, message, w, _, _, gray) = v.image(9, 10_000);
    assert!(status == RenderStatus::Failed && w == 0 && gray.is_empty() && message.contains("no such page"), "{message}");
}

#[test]
fn a_page_too_big_to_be_drawn_for_recognition_is_refused() {
    let dir = out_dir();
    let file = dir.join("huge.pdf");
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 14400 14400] >>".to_string()),
    ];
    std::fs::write(&file, pdf(&objects, "")).unwrap();
    let mut v = Viewer::open(&file);
    let (status, message, ..) = v.image(0, 10_000);
    assert!(status == RenderStatus::Failed && message.contains("too large"), "{message}");
}

/// Five pages of a scan, each of another shape: plain, turned 90, 180 and 270, and a crop box that does not start at 0 (turned 270).
fn shapes() -> Vec<u8> {
    let img = "<< /Type /Page /Parent 2 0 R /Resources << /XObject << /Im0 20 0 R >> >> /Contents 10 0 R";
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 5 /MediaBox [0 0 300 400] >>".to_string()),
        (3, format!("{img} >>")),
        (4, format!("{img} /Rotate 90 >>")),
        (5, format!("{img} /Rotate 180 >>")),
        (6, format!("{img} /Rotate 270 >>")),
        (7, format!("{img} /MediaBox [100 100 400 500] /CropBox [120 130 380 450] /Rotate 270 >>")),
        (10, stream("q 300 0 0 400 0 0 cm /Im0 Do Q")),
        (20, square_image()),
    ];
    pdf(&objects, "")
}

fn check_saved(original: &Path, saved: &Path) {
    let before = std::fs::read(original).unwrap();
    let after = std::fs::read(saved).unwrap();
    assert!(after.len() > before.len() && after.starts_with(&before), "{}: the original bytes are not a prefix", saved.display());
    if let Some(qpdf) = common::find_qpdf() {
        let v = common::qpdf_check(&qpdf, saved);
        assert!(v.code == 0 || v.code == 3, "qpdf --check says {}:\n{}", v.code, v.text);
        let w = common::qpdf_check(&qpdf, original);
        let known = common::qpdf_complaints(&w.text, original);
        let new: Vec<String> = common::qpdf_complaints(&v.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
        assert!(new.is_empty(), "qpdf warns of what the original did not: {new:?}");
    }
}

#[test]
fn the_text_layer_is_where_the_words_are_on_pages_of_every_turn_and_box_and_is_saved_and_undone() {
    let dir = out_dir();
    let input = dir.join("shapes.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let mut v = Viewer::open(&input);
    let doc = Document::open(&input).unwrap();
    let sizes: Vec<(f64, f64)> = doc.pages().unwrap().iter().map(|p| qingpdf_core::text::Shown::of(p).size()).collect();
    // The picture of every page is twice the shown page in pixels. Two lines: three words, and one word; boxes in points of the
    // shown page: (30, 40) to (90, 60) and so on (the line is as high as its words together: 40 to 61).
    let page_of = |n: u32| {
        let (w, h) = sizes[n as usize];
        let (pw, ph) = (w as f32 * 2.0, h as f32 * 2.0);
        OcrPage {
            page: n,
            width: pw as u32,
            height: ph as u32,
            lines: vec![
                words(&[("Hello", [60.0, 80.0, 180.0, 120.0]), ("brave", [200.0, 82.0, 330.0, 122.0]), ("world", [350.0, 80.0, 470.0, 120.0])]),
                words(&[("second", [60.0, 160.0, 250.0, 200.0])]),
            ],
        }
    };
    let result = v.layer((0..5).map(page_of).collect());
    assert_eq!((result.status, result.page), (EditStatus::Done, ALL_PAGES), "{}", result.message);
    assert!(result.can_undo && result.dirty);
    for n in 0..5u32 {
        let (text, boxes) = v.chars(n);
        assert_eq!(text, "Hello brave world\nsecond\n", "page {n}");
        // "Hello" starts at 30 points and "world" ends at 235: the layer's own boxes hold to the words' boxes within a point, in the
        // page as it is shown, whatever the turn and the crop box.
        let first = boxes[0];
        let last_of_line = boxes[text.find('\n').unwrap() - 1];
        assert!((first[0] - 30.0).abs() < 1.0 && (last_of_line[2] - 235.0).abs() < 1.0, "page {n}: {first:?} {last_of_line:?}");
        assert!((first[1] - 40.0).abs() < 1.0 && (first[3] - 61.0).abs() < 1.0, "page {n}: {first:?}");
        let second = boxes[18];
        assert!((second[0] - 30.0).abs() < 1.0 && (second[1] - 80.0).abs() < 1.0 && (second[3] - 100.0).abs() < 1.0, "page {n}: {second:?}");
    }
    assert_eq!(v.found("brave world"), 5, "the search finds the words (across the space of the layer) on every page");
    // Saved: original bytes a prefix, qpdf is content, and the file read again has the same text and boxes.
    let saved = dir.join("shapes-ocr.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&input, &saved);
    let again = Document::open(&saved).unwrap();
    let pages = again.pages().unwrap();
    let mut extractor = TextExtractor::new(&again);
    for (n, p) in pages.iter().enumerate() {
        let chars = extractor.page_chars(p, &Work::new()).unwrap();
        assert_eq!(chars.text, "Hello brave world\nsecond\n", "page {n} of the saved file");
        let (_, live) = v.chars(n as u32);
        for (a, b) in chars.boxes.iter().zip(&live) {
            assert!((0..4).all(|k| (a[k] - b[k]).abs() < 0.01), "page {n}: the saved file puts a character at {a:?}, the open one at {b:?}");
        }
    }
    // The saved file read by the reader: its search finds the words on every page.
    let mut reopened = Viewer::open(&saved);
    assert_eq!(reopened.found("brave world"), 5);
    drop(reopened);
    // Undone, the pages have no text again.
    assert_eq!(v.undo().status, EditStatus::Done);
    assert_eq!(v.chars(0).0, "");
    assert_eq!(v.found("brave"), 0);
}

#[test]
fn a_run_with_nothing_in_it_or_too_much_is_refused_and_leaves_the_file_alone() {
    let dir = out_dir();
    let input = dir.join("limits.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let mut v = Viewer::open(&input);
    let page = |n: u32, lines: Vec<OcrLine>| OcrPage { page: n, width: 600, height: 800, lines };
    let ok = || vec![words(&[("a", [10.0, 10.0, 30.0, 30.0])])];
    for (what, pages, says) in [
        ("no pages", vec![], "no recognised text"),
        ("no words", vec![page(0, vec![]), page(1, vec![words(&[("  ", [1.0, 1.0, 5.0, 5.0])])])], "no text was recognised"),
        ("no such page", vec![page(40, ok())], "no such page"),
        ("a picture with no size", vec![OcrPage { width: 0, ..page(0, ok()) }], "no size"),
        ("too many pages", (0..501).map(|_| page(0, ok())).collect(), "at most"),
        ("too many words in a page", vec![page(0, (0..30_001).map(|_| words(&[("x", [1.0, 1.0, 2.0, 2.0])])).collect())], "at most"),
    ] {
        let r = v.layer(pages);
        assert_eq!(r.status, EditStatus::Failed, "{what}");
        assert!(r.message.contains(says), "{what}: {}", r.message);
    }
    assert!(!v.undo().can_undo, "nothing was left behind");
    // A page named twice, words that are not numbers, a character no font has: done, without a panic.
    let r = v.layer(vec![page(0, ok()), page(0, ok()), page(1, vec![words(&[("b", [f32::NAN, 0.0, 9.0, 9.0]), ("c\u{10FFFF}d", [10.0, 10.0, 90.0, 40.0])])])]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(r.message.contains("left out"), "{}", r.message);
    assert_eq!(v.chars(1).0, "cd\n");
}

#[test]
fn a_file_that_may_not_be_changed_keeps_the_recognised_text_in_memory_only() {
    let dir = out_dir();
    // A signed file (a signature field with a signature in it).
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] /SigFlags 3 >> >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /Resources << /XObject << /Im0 20 0 R >> >> /Contents 11 0 R /Annots [10 0 R] >>".to_string()),
        (10, "<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V 21 0 R /P 3 0 R >>".to_string()),
        (11, stream("q 300 0 0 400 0 0 cm /Im0 Do Q")),
        (20, square_image()),
        (21, "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>".to_string()),
    ];
    let signed = dir.join("signed-scan.pdf");
    std::fs::write(&signed, pdf(&objects, "")).unwrap();
    let mut v = Viewer::open(&signed);
    let page = OcrPage { page: 0, width: 600, height: 800, lines: vec![words(&[("Hello", [60.0, 80.0, 180.0, 120.0]), ("signed", [200.0, 80.0, 330.0, 120.0])])] };
    let r = v.layer(vec![page.clone()]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(r.message.contains("window only"), "{}", r.message);
    assert!(!r.can_undo && !r.dirty, "nothing was written, so nothing is to undo or save");
    // The reader's search, selection and copy use it.
    assert_eq!(v.chars(0).0, "Hello signed\n");
    assert_eq!(v.found("hello signed"), 1);
    // The saved file is the file as it was.
    let saved = dir.join("signed-scan-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let again = Document::open(&saved).unwrap();
    let p = again.pages().unwrap();
    assert_eq!(TextExtractor::new(&again).page_text(&p[0]).unwrap(), "");
    assert_eq!(std::fs::read(&saved).unwrap(), std::fs::read(&signed).unwrap());
    // A file whose author did not allow changing the content: the same.
    if let Some(qpdf) = common::find_qpdf() {
        let input = dir.join("shapes-for-lock.pdf");
        std::fs::write(&input, shapes()).unwrap();
        let locked = dir.join("shapes-nomodify.pdf");
        let status = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner", "256", "--modify=none", "--annotate=y", "--", &input.to_string_lossy(), &locked.to_string_lossy()]).status().unwrap();
        assert!(status.code().is_some_and(|c| c == 0 || c == 3));
        let mut v = Viewer::open(&locked);
        assert!(!v.opened.rights.modify);
        let r = v.layer(vec![OcrPage { page: 2, ..page }]);
        assert_eq!(r.status, EditStatus::Done, "{}", r.message);
        assert!(r.message.contains("window only") && !r.dirty, "{r:?}");
        assert_eq!(v.chars(2).0, "Hello signed\n");
    }
}

#[test]
fn the_biggest_run_is_written_in_seconds_with_one_font_and_stays_within_the_memory_set_aside() {
    // 500 pages of 800 words each: the most a run may hold in a page (the words in all are 400,000).
    let dir = out_dir();
    let input = dir.join("big-run.pdf");
    let kids: String = (0..500).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let mut objects = vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()), (2, format!("<< /Type /Pages /Kids [{kids}] /Count 500 /MediaBox [0 0 300 400] >>"))];
    objects.extend((0..500).map(|i| (10 + i, "<< /Type /Page /Parent 2 0 R >>".to_string())));
    std::fs::write(&input, pdf(&objects, "")).unwrap();
    let mut v = Viewer::open(&input);
    let word_list = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet"];
    let pages: Vec<OcrPage> = (0..500)
        .map(|n| {
            let lines = (0..40)
                .map(|l| OcrLine {
                    words: (0..20).map(|k| OcrWord { text: word_list[(n + l + k) % 10].to_string(), rect: [10.0 + 30.0 * k as f32, 10.0 + 12.0 * l as f32, 36.0 + 30.0 * k as f32, 20.0 + 12.0 * l as f32] }).collect(),
                })
                .collect();
            OcrPage { page: n as u32, width: 600, height: 800, lines }
        })
        .collect();
    let started = std::time::Instant::now();
    let r = v.layer(pages);
    let took = started.elapsed();
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(took.as_secs() < 10, "{took:?}");
    println!("500 pages of 800 words: {took:?}");
    let saved = dir.join("big-run-ocr.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let doc = Document::open(&saved).unwrap();
    assert!((std::fs::read(&saved).unwrap().len() as f64) < 40.0e6);
    let pages = doc.pages().unwrap();
    assert!(TextExtractor::new(&doc).page_text(&pages[499]).unwrap().starts_with("juliet "));
    // Words in all above the limit are refused as a whole.
    let one = |n: u32| OcrPage { page: n, width: 10, height: 10, lines: (0..30).map(|_| OcrLine { words: (0..30_000 / 30).map(|_| OcrWord { text: "a".to_string(), rect: [0.0, 0.0, 1.0, 1.0] }).collect() }).collect() };
    let r = v.layer((0..14).map(one).collect());
    assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    assert!(r.message.contains("at most"), "{}", r.message);
}

/// Not run by default: the search of the reader in a file that was recognised by `qingpdf-view --ocr-test` (the file is `QINGPDF_OCR_FILE`,
/// the words `QINGPDF_OCR_WORDS`, separated by `|`): every word must be found on the page `QINGPDF_OCR_PAGE` (from 1) at least once.
#[test]
#[ignore = "needs QINGPDF_OCR_FILE, QINGPDF_OCR_PAGE and QINGPDF_OCR_WORDS"]
fn the_search_of_the_reader_finds_words_in_a_recognised_file() {
    let file = PathBuf::from(std::env::var("QINGPDF_OCR_FILE").unwrap());
    let page: u32 = std::env::var("QINGPDF_OCR_PAGE").unwrap().parse().unwrap();
    let mut v = Viewer::open(&file);
    for word in std::env::var("QINGPDF_OCR_WORDS").unwrap().split('|') {
        let id = v.id();
        v.engine.search(1, id, word, page - 1);
        let mut on_page = 0;
        v.until(|e| match e {
            Event::SearchHits { id: i, page: p, hits, .. } if i == id => {
                if p == page - 1 {
                    on_page += hits.len();
                }
                None
            }
            Event::SearchDone { id: i, .. } if i == id => Some(()),
            _ => None,
        });
        println!("{word}: {on_page} hits on page {page}");
        assert!(on_page > 0, "{word} is not found on page {page}");
    }
}

/// How many forms of the page's resources carry the mark of a recognised layer.
fn layers_of(doc: &Document, page: usize) -> usize {
    use qingpdf_core::Object;
    let pages = doc.pages().unwrap();
    let Some(Object::Dict(resources)) = pages[page].resources().and_then(|r| doc.resolve(r).ok()) else { return 0 };
    let Some(Object::Dict(forms)) = resources.get("XObject").and_then(|x| doc.resolve(x).ok()) else { return 0 };
    forms.iter().filter(|(_, f)| matches!(doc.resolve(f), Ok(Object::Stream(s)) if matches!(s.dict.get("QingpdfOCR"), Some(Object::Bool(true))))).count()
}

#[test]
fn a_page_with_a_layer_is_done_however_little_it_holds_and_a_second_run_adds_no_second_layer() {
    let dir = out_dir();
    let input = dir.join("twice.pdf");
    std::fs::write(&input, mixed()).unwrap();
    let mut v = Viewer::open(&input);
    assert_eq!(v.scan(None).0, vec![0, 3]);
    // Two characters on page 0: fewer than a page needs to count as having text.
    let one = |n: u32| OcrPage { page: n, width: 600, height: 800, lines: vec![words(&[("ab", [60.0, 80.0, 180.0, 120.0])])] };
    let r = v.layer(vec![one(0)]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert_eq!(v.chars(0).0, "ab\n");
    // The page has its layer: it is not one to recognise again, for the whole file or by itself.
    assert_eq!(v.scan(None).0, vec![3], "page 0 is done, page 3 (a page number only) is not");
    assert_eq!(v.scan(Some(0)).0, Vec::<u32>::new());
    // A run that is given the page again leaves it alone; it says so, and a run of nothing else is refused.
    let r = v.layer(vec![one(0)]);
    assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    assert!(r.message.contains("already"), "{}", r.message);
    let r = v.layer(vec![one(0), one(3)]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(r.message.contains("already had"), "{}", r.message);
    let page3 = v.chars(3).0;
    assert!(v.chars(0).0 == "ab\n" && page3.contains("ab") && page3.contains("12"), "{page3:?}");
    let saved = dir.join("twice-ocr.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let again = Document::open(&saved).unwrap();
    assert_eq!((layers_of(&again, 0), layers_of(&again, 3), layers_of(&again, 1)), (1, 1, 0), "one layer a page");
    // Undone, the page is one to recognise again.
    assert_eq!(v.undo().status, EditStatus::Done);
    assert_eq!(v.undo().status, EditStatus::Done);
    assert_eq!(v.scan(None).0, vec![0, 3]);
}

#[test]
fn the_replace_command_leaves_the_recognised_text_of_a_scan_as_it_is() {
    let dir = out_dir();
    let input = dir.join("replace-input.pdf");
    std::fs::write(&input, shapes()).unwrap();
    let mut v = Viewer::open(&input);
    let page = OcrPage { page: 0, width: 600, height: 800, lines: vec![words(&[("Hello", [60.0, 80.0, 180.0, 120.0]), ("brave", [200.0, 82.0, 330.0, 122.0]), ("world", [350.0, 80.0, 470.0, 120.0])])] };
    assert_eq!(v.layer(vec![page]).status, EditStatus::Done);
    let saved = dir.join("replace-ocr.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let doc = Document::open(&saved).unwrap();
    assert_eq!(TextExtractor::new(&doc).page_text(&doc.pages().unwrap()[0]).unwrap(), "Hello brave world\n");
    // The words are found, but they are the picture's words, and the picture is not changed: nothing is written, and it says why.
    let done = qingpdf_core::edit::replace_text(&doc, "", "brave", "bold", None).unwrap();
    assert_eq!((done.count, done.pages, done.data.len()), (0, 0, 0));
    assert!(done.skipped.iter().any(|(why, n)| why.contains("recognised text") && *n == 1), "{:?}", done.skipped);
}

/// Six pages that share what a mark is put into: two share a `/Resources` object, two inherit one from the page tree, two have their own
/// `/Resources` whose `/XObject` is a dictionary that is an object shared by both.
fn sharing() -> Vec<u8> {
    let objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] /Count 6 /MediaBox [0 0 300 300] /Resources 20 0 R >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /Resources 21 0 R >>".to_string()),
        (4, "<< /Type /Page /Parent 2 0 R /Resources 21 0 R >>".to_string()),
        (5, "<< /Type /Page /Parent 2 0 R >>".to_string()),
        (6, "<< /Type /Page /Parent 2 0 R >>".to_string()),
        (7, "<< /Type /Page /Parent 2 0 R /Resources << /XObject 22 0 R >> >>".to_string()),
        (8, "<< /Type /Page /Parent 2 0 R /Resources << /XObject 22 0 R >> >>".to_string()),
        (20, "<< /ProcSet [/PDF] >>".to_string()),
        (21, "<< /ProcSet [/PDF] >>".to_string()),
        (22, "<< >>".to_string()),
    ];
    pdf(&objects, "")
}

#[test]
fn pages_that_share_their_resources_each_get_the_text_layer_of_their_own() {
    let dir = out_dir();
    let input = dir.join("sharing.pdf");
    std::fs::write(&input, sharing()).unwrap();
    let mut v = Viewer::open(&input);
    let names = ["ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT"];
    let page_of = |n: u32| OcrPage { page: n, width: 600, height: 600, lines: vec![words(&[(names[n as usize], [60.0, 80.0, 300.0, 120.0])])] };
    let result = v.layer((0..6).map(page_of).collect());
    assert_eq!((result.status, result.page), (EditStatus::Done, ALL_PAGES), "{}", result.message);
    for n in 0..6u32 {
        assert_eq!(v.chars(n).0, format!("{}\n", names[n as usize]), "page {n} of the open file");
    }
    let saved = dir.join("sharing-ocr.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&input, &saved);
    let again = Document::open(&saved).unwrap();
    let mut extractor = TextExtractor::new(&again);
    for (n, p) in again.pages().unwrap().iter().enumerate() {
        assert_eq!(extractor.page_chars(p, &Work::new()).unwrap().text, format!("{}\n", names[n]), "page {n} of the saved file");
    }
    // What the pages share is not changed, and what they are given is a copy for each (the three kinds of sharing: 21, 20, 22).
    for (num, same) in [(20u32, "<< /ProcSet [/PDF] >>"), (21, "<< /ProcSet [/PDF] >>")] {
        let o = again.get(qingpdf_core::object::ObjRef::new(num, 0)).unwrap();
        assert!(matches!(&o, qingpdf_core::object::Object::Dict(d) if d.len() == 1), "object {num} ({same}) is as it was: {o:?}");
    }
}
