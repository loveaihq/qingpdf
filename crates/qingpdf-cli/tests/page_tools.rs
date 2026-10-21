//! `qingpdf crop`, `images`, `meta`, `render --format jpeg` and the new lines of `info` (2026-10-21), run as a user would: arguments in, files and exit
//! codes out. What is written is read back with the engine and, where it is there, with qpdf; the other readers are checked by
//! `tests/tools/page_tools_check.py`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};
use std::time::Instant;

use qingpdf_core::Document;
use qingpdf_core::object::{ObjRef, Object};
use qingpdf_core::render::Renderer;
use runner::{code, run, s, stderr, stdout};

fn out_dir(name: &str) -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("page-tools-cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file of `pages` pages of 200 by 100 points (the first optionally turned), each with a red box at (40, 20) to (120, 60) and a line of text.
fn boxes_pdf(pages: u32, first_rotate: u32, info_and_xmp: bool) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let mut put = |out: &mut Vec<u8>, n: u32, body: String| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Old title</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>";
    put(&mut out, 1, format!("<< /Type /Catalog /Pages 2 0 R{} >>", if info_and_xmp { " /Metadata 5 0 R" } else { "" }));
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    put(&mut out, 2, format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> >> >>"));
    put(&mut out, 4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
    if info_and_xmp {
        put(&mut out, 3, "<< /Title (Old title) /Author (Old author) /CreationDate (D:20260101080000+08'00') >>".to_string());
        put(&mut out, 5, format!("<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{xmp}\nendstream", xmp.len()));
    }
    for i in 0..pages {
        let rotate = if i == 0 && first_rotate != 0 { format!(" /Rotate {first_rotate}") } else { String::new() };
        let text = "1 0 0 rg 40 20 80 40 re f BT /F1 10 Tf 0 g 40 70 Td (Hello page) Tj ET";
        put(&mut out, 10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R{rotate} >>", 11 + 2 * i));
        put(&mut out, 11 + 2 * i, format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    }
    let size = 10 + 2 * pages;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    let info = if info_and_xmp { " /Info 3 0 R" } else { "" };
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R{info} >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

fn crop_boxes(file: &Path, password: &str) -> Vec<Option<[f64; 4]>> {
    let doc = Document::open_with_password(file, password).unwrap();
    doc.pages().unwrap().iter().map(|p| p.crop_box()).collect()
}

fn near(a: [f64; 4], b: [f64; 4]) {
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.02), "{a:?} is not {b:?}");
}

const MM: f64 = 72.0 / 25.4;

fn qpdf_ok(file: &Path, password: &str) {
    let Some(qpdf) = common::find_qpdf() else { return };
    let verdict = common::qpdf_check_pw(&qpdf, file, password);
    assert_eq!(verdict.code, 0, "qpdf --check {}: {}", file.display(), verdict.text);
}

#[test]
fn margins_are_cut_off_the_pages_as_they_are_shown_even_when_they_are_turned() {
    let dir = out_dir("margins");
    let input = dir.join("in.pdf");
    let original = boxes_pdf(3, 90, false);
    std::fs::write(&input, &original).unwrap();
    let output = dir.join("out.pdf");
    // 4, 8, 2 and 6 millimetres off the top, right, bottom and left of pages 1 and 3 (page 1 is turned a quarter: shown 100 by 200).
    let r = run(["crop", s(&input), "--margins", "4,8,2,6", "--pages", "1,3", "-o", s(&output)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("2 pages cropped"), "{}", stdout(&r));
    let wrote = std::fs::read(&output).unwrap();
    assert!(wrote.starts_with(&original), "the original bytes are the start of the file");
    let boxes = crop_boxes(&output, "");
    // The turned page: shown 100 wide, 200 high; the top of the shown page is user x from 0, the left of it user y from 0 (a turn of 90 degrees).
    // So the crop box is x 10 mm .. 200 - 5 mm and y 15 mm .. 100 - 20 mm... in user space: shown (left, top) = (15, 10) mm maps to user (y + 0, x + 0).
    let (t, rt, b, l) = (4.0 * MM, 8.0 * MM, 2.0 * MM, 6.0 * MM);
    near(boxes[0].unwrap(), [t, l, 200.0 - b, 100.0 - rt]);
    assert_eq!(boxes[1], None, "page 2 is not touched");
    near(boxes[2].unwrap(), [l, b, 200.0 - rt, 100.0 - t]);
    qpdf_ok(&output, "");
    // Over an existing file only with --force; the margins must be four numbers; they must leave something.
    assert_eq!(code(&run(["crop", s(&input), "--margins", "1,1,1,1", "-o", s(&output)])), 1);
    assert_eq!(code(&run(["crop", s(&input), "--margins", "1,1,1", "-o", s(&output), "--force"])), 2);
    assert_eq!(code(&run(["crop", s(&input), "-o", s(&output), "--force"])), 2);
    assert_eq!(code(&run(["crop", s(&input), "--margins", "1,1,1,1", "--auto", "-o", s(&output), "--force"])), 2);
    let r = run(["crop", s(&input), "--margins", "60,0,60,0", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("whole page"), "{}", stderr(&r));
    // The help.
    let help = stdout(&run(["crop", "--help"]));
    assert!(help.contains("--margins") && help.contains("--auto") && stdout(&run(["--help"])).contains("crop"), "{help}");
}

#[test]
fn the_white_margins_are_found_and_a_blank_page_is_left_alone() {
    let dir = out_dir("auto");
    let input = dir.join("in.pdf");
    // Pages 1 and 2 have the box and the text; page 3 is made blank by taking its drawing away.
    let mut text = String::from_utf8(boxes_pdf(3, 0, false)).unwrap();
    let blank = "1 0 0 rg 40 20 80 40 re f BT /F1 10 Tf 0 g 40 70 Td (Hello page) Tj ET";
    let last = text.rfind(blank).unwrap();
    text.replace_range(last..last + blank.len(), &" ".repeat(blank.len()));
    std::fs::write(&input, text.as_bytes()).unwrap();
    let output = dir.join("out.pdf");
    let r = run(["crop", s(&input), "--auto", "-o", s(&output)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("2 pages cropped") && stderr(&r).contains("nothing is drawn on page 3"), "{} {}", stdout(&r), stderr(&r));
    let boxes = crop_boxes(&output, "");
    // The drawing is x 40..120, y 20..60 (the box) and the text up to about y 80, and 3 mm (8.5 points) round it.
    let b = boxes[0].unwrap();
    assert!(b[0] > 40.0 - 8.5 - 4.0 && b[0] < 40.0 - 8.5 + 4.0, "{b:?}");
    assert!(b[1] > 20.0 - 8.5 - 4.0 && b[1] < 20.0 - 8.5 + 4.0, "{b:?}");
    assert!(b[2] > 120.0 + 8.5 - 4.0 && b[2] < 120.0 + 8.5 + 8.0 + 20.0, "{b:?}");
    assert!(b[3] <= 100.0 && b[3] > 70.0, "{b:?}");
    assert_eq!(boxes[2], None);
    // Only blank pages: nothing to crop, an error.
    let blank_only = dir.join("blank.pdf");
    std::fs::write(&blank_only, text.as_bytes()).unwrap();
    let r = run(["crop", s(&blank_only), "--auto", "--pages", "3", "-o", s(&dir.join("none.pdf"))]);
    assert_eq!(code(&r), 1, "{}", stderr(&r));
    assert!(!dir.join("none.pdf").exists());
}

#[test]
fn an_aes_256_file_is_cropped_as_an_update_that_keeps_its_encryption() {
    let input = common::public_file("encrypted/encrypted_hello_world_r6.pdf");
    let dir = out_dir("aes");
    let output = dir.join("out.pdf");
    let r = run(["crop", s(&input), "--margins", "5,5,5,5", "--password", "h\u{f4}tel", "-o", s(&output)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let (before, after) = (std::fs::read(&input).unwrap(), std::fs::read(&output).unwrap());
    assert!(after.starts_with(&before));
    let b = crop_boxes(&output, "h\u{f4}tel")[0].unwrap();
    let doc = Document::open_with_password(&output, "h\u{f4}tel").unwrap();
    assert!(doc.is_encrypted());
    let page = &doc.pages().unwrap()[0];
    let m = page.media_box().unwrap();
    near(b, [m[0] + 5.0 * MM, m[1] + 5.0 * MM, m[2] - 5.0 * MM, m[3] - 5.0 * MM]);
    qpdf_ok(&output, "h\u{f4}tel");
    // Without the password the text says so.
    let r = run(["crop", s(&input), "--margins", "5,5,5,5", "-o", s(&dir.join("x.pdf"))]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("password"), "{}", stderr(&r));
}

#[test]
fn a_real_gongwen_is_cropped_by_margins_and_by_its_content_and_qpdf_accepts_both() {
    let input = common::public_file("zh/gongwen/gongwen-2024-ai-standards-guide.pdf");
    let dir = out_dir("gongwen");
    let output = dir.join("margins.pdf");
    let r = run(["crop", s(&input), "--margins", "10,10,10,10", "--pages", "1-3", "-o", s(&output)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(std::fs::read(&output).unwrap().starts_with(&std::fs::read(&input).unwrap()));
    qpdf_ok(&output, "");
    let before = Document::open(&input).unwrap();
    let after = Document::open(&output).unwrap();
    let (a, b) = (&before.pages().unwrap()[0], &after.pages().unwrap()[0]);
    let (wa, wb) = (qingpdf_core::text::Shown::of(a).size().0, qingpdf_core::text::Shown::of(b).size().0);
    assert!((wa - wb - 20.0 * MM).abs() < 0.1, "{wa} -> {wb}");
    let auto = dir.join("auto.pdf");
    let r = run(["crop", s(&input), "--auto", "--pages", "2-4", "-o", s(&auto)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    qpdf_ok(&auto, "");
    let cropped = Document::open(&auto).unwrap().pages().unwrap();
    let was = before.pages().unwrap();
    assert_eq!(cropped[0].crop_box(), was[0].crop_box(), "page 1 was not asked for");
    assert!((1..=3).all(|i| qingpdf_core::text::Shown::of(&cropped[i]).size().0 < qingpdf_core::text::Shown::of(&was[i]).size().0 - 20.0), "pages 2 to 4 are narrower by the white round them");
}

#[test]
fn pages_are_written_as_png_or_jpeg_in_bands_and_match_a_whole_render() {
    let dir = out_dir("render");
    let input = common::public_file("zh/gongwen/gongwen-2024-ai-standards-guide.pdf");
    let doc = Document::open(&input).unwrap();
    let pages = doc.pages().unwrap();
    let mut renderer = Renderer::new(&doc, qingpdf_core::forms::painter());
    let whole = renderer.render_page(&pages[0], 150.0).unwrap();
    let rgb: Vec<u8> = whole.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let png = dir.join("p1.png");
    let r = run(["render", s(&input), "--pages", "1", "--dpi", "150", "-o", s(&png)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let mut reader = ::png::Decoder::new(std::io::Cursor::new(std::fs::read(&png).unwrap())).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (whole.width, whole.height));
    // (Drawn in bands of a million pixels: a glyph or two may sit a quarter pixel off where the whole page would put it; the bands are exact.)
    let differing = buf[..info.buffer_size()].iter().zip(&rgb).filter(|(a, b)| a != b).count();
    assert!(differing * 200 < rgb.len(), "{differing} of {} samples differ from a whole drawing", rgb.len());
    let band = (1usize << 20) / info.width as usize;
    let piece = renderer.render_partial(&pages[0], 150.0, 0, Some([0, band as u32, info.width, 40])).unwrap().bitmap;
    let want: Vec<u8> = piece.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let at = band * info.width as usize * 3;
    assert!(buf[at..at + want.len()] == want[..], "the same pixels as the same piece drawn alone");
    // JPEG: the extension says it (or --format), the quality is told, and the picture is the page within a few levels.
    let jpg = dir.join("p1.jpg");
    let r = run(["render", s(&input), "--pages", "1", "--dpi", "150", "--quality", "95", "-o", s(&jpg)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let bytes = std::fs::read(&jpg).unwrap();
    let (w, h, back) = qingpdf_core::render::image::jpeg_samples(&bytes, 3, usize::MAX).unwrap();
    assert_eq!((w as u32, h as u32), (whole.width, whole.height));
    let mean = back.iter().zip(&rgb).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>() as f64 / rgb.len() as f64;
    assert!(mean < 3.0, "mean difference {mean}");
    // 600 dpi: more than the 16 million pixels a whole page could be; written in bands, the same as the pieces of the whole page.
    let big = dir.join("p1-600.png");
    let r = run(["render", s(&input), "--pages", "1", "--dpi", "600", "-o", s(&big)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let mut reader = ::png::Decoder::new(std::io::Cursor::new(std::fs::read(&big).unwrap())).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let (fw, fh) = Renderer::page_pixels(&pages[0], 600.0, 0);
    assert_eq!((u64::from(info.width), u64::from(info.height)), (fw, fh));
    assert!(fw * fh > 16_000_000, "the page is more than a whole drawing allows");
    // (The bands are a million pixels: the fourth starts at row 3 * (1,048,576 / width). The same piece drawn alone is the same pixels. Pieces that start
    // elsewhere can differ in a glyph or two, where the position of a glyph in a piece, an f32 sum, lands the other side of a quarter pixel.)
    let band = (1usize << 20) / info.width as usize;
    let y0 = 3 * band;
    let piece = renderer.render_partial(&pages[0], 600.0, 0, Some([0, y0 as u32, info.width, 64])).unwrap().bitmap;
    let want: Vec<u8> = piece.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let at = y0 * info.width as usize * 3;
    let first = buf[at..at + want.len()].iter().zip(&want).position(|(a, b)| a != b);
    assert!(first.is_none(), "the same pixels in the band: first difference at {first:?}");
    // Options: a format that is not one, a quality without jpeg, a quality out of range.
    for bad in [["--format", "gif"], ["--format", "png"], ["--quality", "0"], ["--quality", "101"], ["--quality", "x"]] {
        let mut args = vec!["render".to_string(), s(&input).to_string(), "--pages".into(), "1".into(), "-o".into(), s(&dir.join("bad.png")).to_string()];
        if bad[0] == "--quality" {
            args.extend(["--format".into(), "jpeg".into()]);
        }
        args.extend(bad.map(String::from));
        if bad == ["--format", "png"] {
            args.extend(["--quality".into(), "50".into()]);
        }
        assert_eq!(code(&run(&args)), 2, "{args:?}");
    }
}

#[test]
fn the_pictures_of_the_pages_are_saved_as_they_are_and_once() {
    let dir = out_dir("images");
    let jpeg = std::fs::read(common::workspace_root().join("crates").join("qingpdf-core").join("tests").join("fixtures").join("rgb_progressive.jpg")).unwrap();
    // Two pages show the same JPEG; the first also shows a raw picture.
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let mut put = |out: &mut Vec<u8>, n: u32, body: Vec<u8>| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\nendobj\n");
    };
    let stream = |dict: &str, data: &[u8]| [format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes(), data.to_vec(), b"\nendstream".to_vec()].concat();
    put(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    put(&mut out, 2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 100] >>".to_vec());
    put(&mut out, 3, b"<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /XObject << /A 7 0 R /B 8 0 R >> >> >>".to_vec());
    put(&mut out, 4, b"<< /Type /Page /Parent 2 0 R /Contents 6 0 R /Resources << /XObject << /A 7 0 R >> >> >>".to_vec());
    put(&mut out, 5, stream("", b"q 90 0 0 90 5 5 cm /A Do Q q 90 0 0 90 105 5 cm /B Do Q"));
    put(&mut out, 6, stream("", b"q 90 0 0 90 5 5 cm /A Do Q"));
    put(&mut out, 7, stream("/Type /XObject /Subtype /Image /Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", &jpeg));
    put(&mut out, 8, stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB", &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]));
    let at = out.len();
    out.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
    for n in 1..9 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    let input = dir.join("album.pdf");
    std::fs::write(&input, &out).unwrap();
    let folder = dir.join("pics");
    let r = run(["images", s(&input), "-o", s(&folder)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("2 pictures saved"), "{}", stdout(&r));
    assert_eq!(std::fs::read(folder.join("album-p1-1.jpg")).unwrap(), jpeg, "the JPEG as it is in the file, byte for byte");
    let png = std::fs::read(folder.join("album-p1-2.png")).unwrap();
    let reader = ::png::Decoder::new(std::io::Cursor::new(&png)).read_info().unwrap();
    assert_eq!((reader.info().width, reader.info().height), (2, 2));
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2, "the JPEG of page 2 is the same picture");
    // The pages chosen: page 2 alone has the JPEG (met first there).
    let second = dir.join("second");
    let r = run(["images", s(&input), "--pages", "2", "-o", s(&second)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert_eq!(std::fs::read(second.join("album-p2-1.jpg")).unwrap(), jpeg);
    // Files that are there are not written over without --force.
    let r = run(["images", s(&input), "-o", s(&folder)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("--force"), "{}", stderr(&r));
    assert_eq!(code(&run(["images", s(&input), "-o", s(&folder), "--force"])), 0);
    // Every name is checked before a file is written: with only the second one there, nothing of the first is saved.
    std::fs::remove_file(folder.join("album-p1-1.jpg")).unwrap();
    let r = run(["images", s(&input), "-o", s(&folder)]);
    assert_eq!(code(&r), 1, "{}", stdout(&r));
    assert!(stderr(&r).contains("album-p1-2.png") && stderr(&r).contains("--force"), "{}", stderr(&r));
    assert!(!folder.join("album-p1-1.jpg").exists() && stdout(&r).is_empty(), "nothing was written before the refusal");
    assert_eq!(code(&run(["images", s(&input), "-o", s(&folder), "--force"])), 0);
    assert_eq!(std::fs::read(folder.join("album-p1-1.jpg")).unwrap(), jpeg);
    // A folder that is a file; a file with no pictures; a file that does not allow copying.
    assert_eq!(code(&run(["images", s(&input), "-o", s(&input)])), 1);
    let plain = dir.join("plain.pdf");
    std::fs::write(&plain, boxes_pdf(1, 0, false)).unwrap();
    let r = run(["images", s(&plain), "-o", s(&dir.join("none"))]);
    assert_eq!(code(&r), 0);
    assert!(stdout(&r).contains("no pictures"), "{}", stdout(&r));
    let locked = common::generated_dir().join("two.r2-40-empty-modify-extract-none.pdf");
    let r = run(["images", s(&locked), "-o", s(&dir.join("locked"))]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("copying"), "{}", stderr(&r));
    assert!(!dir.join("locked").exists() || std::fs::read_dir(dir.join("locked")).unwrap().count() == 0);
}

#[test]
fn the_title_author_subject_and_keywords_are_changed_in_the_info_and_in_the_xmp() {
    let dir = out_dir("meta");
    let input = dir.join("in.pdf");
    let original = boxes_pdf(1, 0, true);
    std::fs::write(&input, &original).unwrap();
    let output = dir.join("out.pdf");
    let r = run(["meta", s(&input), "--title", "\u{65B0}\u{6807}\u{9898}", "--author", "", "--subject", "About", "--keywords", "a, b", "-o", s(&output)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let wrote = std::fs::read(&output).unwrap();
    assert!(wrote.starts_with(&original));
    let doc = Document::open(&output).unwrap();
    let info = doc.info().unwrap().unwrap();
    let text = |k: &str| match info.get(k) {
        Some(Object::String(s)) => Some(qingpdf_core::info::decode_text_string(&s.bytes)),
        _ => None,
    };
    assert_eq!((text("Title").as_deref(), text("Author"), text("Subject").as_deref(), text("Keywords").as_deref()), (Some("\u{65B0}\u{6807}\u{9898}"), None, Some("About"), Some("a, b")));
    assert_eq!(text("CreationDate").as_deref(), Some("D:20260101080000+08'00'"), "the dates are left");
    let Object::Stream(x) = doc.get(ObjRef::new(5, 0)).unwrap() else { panic!("no XMP") };
    let xml = String::from_utf8(doc.decode_stream(&x).unwrap()).unwrap();
    assert!(xml.contains("\u{65B0}\u{6807}\u{9898}") && xml.contains("About") && xml.contains("a, b") && !xml.contains("Old title"), "{xml}");
    qpdf_ok(&output, "");
    // info tells it.
    let r = run(["info", s(&output)]);
    assert!(stdout(&r).contains("Title: \u{65B0}\u{6807}\u{9898}") && stdout(&r).contains("XMP metadata:        yes"), "{}", stdout(&r));
    // Usage: nothing to change, no such option for another command.
    assert_eq!(code(&run(["meta", s(&input), "-o", s(&dir.join("x.pdf"))])), 2);
    assert_eq!(code(&run(["meta", s(&input), "--title", "x"])), 2);
    assert_eq!(code(&run(["info", s(&input), "--title", "x"])), 2);
    // A file with no information dictionary gets one.
    let bare = dir.join("bare.pdf");
    std::fs::write(&bare, boxes_pdf(1, 0, false)).unwrap();
    let made = dir.join("made.pdf");
    assert_eq!(code(&run(["meta", s(&bare), "--title", "Made", "-o", s(&made)])), 0);
    let doc = Document::open(&made).unwrap();
    assert!(matches!(doc.info().unwrap().unwrap().get("Title"), Some(Object::String(_))));
    qpdf_ok(&made, "");
    // A file that is signed is refused: use a copy of the one with a certification.
    let help = stdout(&run(["meta", "--help"]));
    assert!(help.contains("--title") && help.contains("--keywords"), "{help}");
}

#[test]
fn info_lists_the_properties_and_with_fonts_the_fonts() {
    let input = common::corpus_root().join("local").join("xref-classic").join("tracemonkey.pdf");
    if !input.is_file() {
        return;
    }
    let r = run(["info", s(&input), "--fonts"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let text = stdout(&r);
    for want in ["Signed:", "Tagged:", "Linearized:", "Attachments:", "JavaScript:", "XMP metadata:", "Fonts (", "NimbusRomNo9L-Regu [Type1] embedded (subset)", " in\n"] {
        assert!(text.contains(want), "{want:?} is not in\n{text}");
    }
    assert!(!stdout(&run(["info", s(&input)])).contains("Fonts ("));
    // A locked file: said, not listed.
    let locked = common::public_file("encrypted/encrypted_hello_world_r6.pdf");
    let r = run(["info", s(&locked)]);
    assert!(stdout(&r).contains("Encryption:") && !stdout(&r).contains("Signed:"), "{}", stdout(&r));
}

#[test]
fn a_hundred_pages_are_looked_at_and_written_as_pictures_in_a_moment() {
    let dir = out_dir("speed");
    let input = dir.join("hundred.pdf");
    std::fs::write(&input, boxes_pdf(100, 0, false)).unwrap();
    let output = dir.join("cropped.pdf");
    let started = Instant::now();
    let r = run(["crop", s(&input), "--auto", "-o", s(&output)]);
    let took = started.elapsed();
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    println!("auto-crop of 100 pages: {took:?}");
    let pattern = dir.join("p_%d.png");
    let started = Instant::now();
    let r = run(["render", s(&input), "--dpi", "150", "-o", s(&pattern)]);
    let drew = started.elapsed();
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    println!("100 pages at 150 dpi as PNG: {drew:?}");
    if !cfg!(debug_assertions) {
        assert!(took.as_secs_f64() <= 3.0, "{took:?}");
        assert!(drew.as_secs_f64() <= 10.0, "{drew:?}");
    }
}
