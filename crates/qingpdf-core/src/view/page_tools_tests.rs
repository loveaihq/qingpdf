//! Tests of the requests of 2026-10-21: cropping pages, looking for the white margins, the pages and pictures as files, the properties.

use std::path::PathBuf;

use super::tests::{open, pdf, square};
use super::*;
use crate::edit::{CropOp, Meta};
use crate::object::{ObjRef, Object};
use crate::testutil::PdfBuilder;

/// Events until `pick` finds one it wants (up to ten seconds for each).
fn until<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        match engine.wait(10_000) {
            Some(e) => {
                if let Some(t) = pick(e) {
                    return t;
                }
            }
            None => panic!("the engine said nothing for ten seconds"),
        }
    }
}

fn edited(engine: &Engine, id: u64) -> EditResult {
    until(engine, |e| match e {
        Event::Edited(r) if r.id == id => Some(r),
        _ => None,
    })
}

fn sizes(r: &EditResult) -> Vec<(f64, f64)> {
    r.pages.as_ref().expect("the list of pages").iter().map(|p| (p.width, p.height)).collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qingpdf-page-tools-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder");
    dir
}

/// Pages 200 by 100 (the third turned a quarter, so 100 by 200 as shown), a red square on each.
fn three_pages() -> Vec<u8> {
    pdf(3, square)
}

#[test]
fn cropping_changes_the_sizes_of_the_pages_and_undo_and_uncrop_give_them_back() {
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, three_pages());
    assert!(o.rights.assemble && o.rights.modify);
    // The top left quarter of page 0 and the lower right quarter of the turned page 2 (shown 100 by 200).
    engine.crop_pages(1, 1, CropOp::Set(vec![(0, [0.0, 0.0, 100.0, 50.0]), (2, [50.0, 100.0, 100.0, 200.0])]));
    let r = edited(&engine, 1);
    assert_eq!((r.status, r.page), (EditStatus::Done, crate::edit::ALL_PAGES));
    assert_eq!(sizes(&r), [(100.0, 50.0), (200.0, 100.0), (50.0, 100.0)]);
    assert!(r.dirty && r.can_undo);
    // A page that is not there, and a rectangle that is not on the page: not done, and the pages are as they were.
    engine.crop_pages(1, 2, CropOp::Set(vec![(7, [0.0, 0.0, 10.0, 10.0])]));
    assert_eq!(edited(&engine, 2).status, EditStatus::Failed);
    engine.crop_pages(1, 3, CropOp::Set(vec![(1, [500.0, 500.0, 600.0, 600.0])]));
    assert_eq!(edited(&engine, 3).status, EditStatus::Failed);
    // Undo: the sizes of the file; redo; uncrop of page 0 only.
    engine.undo(1, 4);
    assert_eq!(sizes(&edited(&engine, 4)), [(200.0, 100.0), (200.0, 100.0), (100.0, 200.0)]);
    engine.redo(1, 5);
    assert_eq!(sizes(&edited(&engine, 5)), [(100.0, 50.0), (200.0, 100.0), (50.0, 100.0)]);
    engine.crop_pages(1, 6, CropOp::Reset(vec![0]));
    assert_eq!(sizes(&edited(&engine, 6)), [(200.0, 100.0), (200.0, 100.0), (50.0, 100.0)]);
    // Nothing is cropped on page 1: nothing to give back.
    engine.crop_pages(1, 7, CropOp::Reset(vec![1]));
    assert_eq!(edited(&engine, 7).status, EditStatus::Failed);
    // The cropped page is drawn as the part of the page that is kept.
    engine.render(RenderRequest { doc: 1, id: 8, page: 2, dpi: 72.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
    let drawn = until(&engine, |e| if let Event::Rendered(r) = e { Some(r) } else { None });
    assert_eq!((drawn.width, drawn.height), (50, 100));
}

#[test]
fn a_file_that_does_not_allow_changing_its_pages_is_not_cropped() {
    use crate::security::{Method, Permissions, Protection};
    let doc = Document::from_bytes(three_pages()).unwrap();
    let permissions = Permissions { print: true, print_high_quality: true, modify: false, copy: false, annotate: true, fill_forms: true, assemble: false, accessibility: true };
    let protection = Protection { method: Method::AesV3, user_password: String::new(), owner_password: "owner".to_string(), permissions, encrypt_metadata: true };
    let locked = crate::ops::protect(&doc, "", &protection, &[7u8; 32]).unwrap().data;
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, locked.clone());
    assert!(!o.rights.assemble && !o.rights.modify && !o.rights.copy);
    engine.crop_pages(1, 1, CropOp::Set(vec![(0, [0.0, 0.0, 100.0, 50.0])]));
    assert_eq!(edited(&engine, 1).status, EditStatus::Denied);
    engine.set_properties(1, 2, Meta { title: Some("T".to_string()), ..Meta::default() });
    assert_eq!(edited(&engine, 2).status, EditStatus::Denied);
    // Nor are the pages written as pictures or the pictures taken out: copying is not allowed.
    let dir = scratch("denied");
    engine.export_images(1, 3, &dir.to_string_lossy(), "locked", vec![0], (72.0, ImageFormat::Png));
    let (status, message) = until(&engine, |e| if let Event::Exported { id: 3, status, message, .. } = e { Some((status, message)) } else { None });
    assert_eq!(status, SaveStatus::Failed);
    assert!(message.contains("copying"), "{message}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    // With the owner password everything is allowed.
    engine.open_bytes(2, locked, "owner", TOTAL_BYTES, 1920 * 1080);
    let owner = until(&engine, |e| if let Event::Opened(o) = e { Some(o) } else { None });
    assert!(owner.rights.assemble && owner.rights.modify && owner.rights.copy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_white_margins_are_found_by_drawing_the_pages_small() {
    let engine = Engine::start(Box::new(|| {}));
    // Page 0: a square at the lower left; page 1: nothing; page 2 (turned): the same square.
    open(&engine, pdf(3, |i| if i == 1 { String::new() } else { "1 0 0 rg 0 0 20 20 re f".to_string() }));
    engine.auto_crop(1, 1, vec![0, 1, 2, 2]);
    let (boxes, failed) = until(&engine, |e| if let Event::AutoCrop { id: 1, boxes, failed, .. } = e { Some((boxes, failed)) } else { None });
    assert!(failed.is_empty());
    assert_eq!(boxes.iter().map(|b| b.0).collect::<Vec<_>>(), [0, 1, 2], "each page once");
    // Page 0 (200 by 100 as shown): the square is at the lower left, 20 points a side; 3 mm (8.5 points) round it, kept on the page.
    let near = |a: f32, b: f32| (a - b).abs() < 4.0;
    let r = boxes[0].1.expect("something on the page");
    assert!(near(r[0], 0.0) && near(r[1], 71.5) && near(r[2], 28.5) && near(r[3], 100.0), "{r:?}");
    assert_eq!(boxes[1].1, None, "a blank page");
    // The turned page is 100 by 200 as shown; the square is at its upper left (user space x 0..20, y 0..20 turns to the top left).
    let r = boxes[2].1.expect("something on the page");
    assert!(near(r[0], 0.0) && near(r[1], 0.0) && near(r[2], 28.5) && near(r[3], 28.5), "{r:?}");
    // Cropping by what was found.
    engine.crop_pages(1, 2, CropOp::Set(vec![(0, boxes[0].1.unwrap())]));
    let done = edited(&engine, 2);
    let (w, h) = sizes(&done)[0];
    assert!((w - 28.5).abs() < 4.0 && (h - 28.5).abs() < 4.0, "{w} {h}");
}

/// A document of two pages that show the same JPEG (the first also a raw picture with a soft mask).
fn pictures_pdf(jpeg: &[u8]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 5 0 R /Resources << /XObject << /A 7 0 R /B 8 0 R >> >> >>");
    b.obj(4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 6 0 R /Resources << /XObject << /A 7 0 R >> >> >>");
    b.stream_obj(5, "", b"q 90 0 0 90 5 5 cm /A Do Q q 90 0 0 90 105 5 cm /B Do Q");
    b.stream_obj(6, "", b"q 90 0 0 90 5 5 cm /A Do Q");
    b.stream_obj(7, "/Type /XObject /Subtype /Image /Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", jpeg);
    b.stream_obj(8, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 9 0 R", &[0, 85, 170, 255]);
    b.stream_obj(9, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[255, 128, 64, 0]);
    b.finish_classic(10, "/Root 1 0 R")
}

#[test]
fn pages_are_written_as_pictures_and_the_pictures_of_pages_are_saved_as_they_are() {
    let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).unwrap();
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, pictures_pdf(&jpeg));
    let dir = scratch("pictures");
    let d = dir.to_string_lossy().into_owned();
    // Both pages as PNG at 144 dpi, and page 2 again as a JPEG.
    engine.export_images(1, 1, &d, "book", vec![1, 0, 1], (144.0, ImageFormat::Png));
    let (status, message, files) = until(&engine, |e| if let Event::Exported { id: 1, status, message, pages, .. } = e { Some((status, message, pages)) } else { None });
    assert_eq!((status, message.as_str(), files), (SaveStatus::Done, "", 2));
    engine.export_images(1, 2, &d, "book", vec![1], (144.0, ImageFormat::Jpeg(80)));
    until(&engine, |e| if let Event::Exported { id: 2, status: SaveStatus::Done, .. } = e { Some(()) } else { None });
    let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["book-1.png", "book-2.jpg", "book-2.png"]);
    let png = std::fs::read(dir.join("book-1.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let reader = ::png::Decoder::new(std::io::Cursor::new(&png)).read_info().unwrap();
    assert_eq!((reader.info().width, reader.info().height), (400, 200));
    assert_eq!(&std::fs::read(dir.join("book-2.jpg")).unwrap()[..2], &[0xFF, 0xD8]);
    // The pictures: the JPEG is saved once (from page 1, where it is met first), as it is; the other as a PNG.
    let out = scratch("pictures-out");
    engine.extract_images(1, 3, &out.to_string_lossy(), "book", vec![0, 1]);
    let (status, files) = until(&engine, |e| if let Event::Exported { id: 3, status, pages, .. } = e { Some((status, pages)) } else { None });
    assert_eq!((status, files), (SaveStatus::Done, 2));
    assert_eq!(std::fs::read(out.join("book-p1-1.jpg")).unwrap(), jpeg);
    let alpha = std::fs::read(out.join("book-p1-2.png")).unwrap();
    let mut reader = ::png::Decoder::new(std::io::Cursor::new(&alpha)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height, info.color_type), (2, 2, ::png::ColorType::Rgba));
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
    // Taking the pictures out again never takes the place of a file that is there: the new ones get -2 after the name.
    engine.extract_images(1, 7, &out.to_string_lossy(), "book", vec![0]);
    let (status, files) = until(&engine, |e| if let Event::Exported { id: 7, status, pages, .. } = e { Some((status, pages)) } else { None });
    assert_eq!((status, files), (SaveStatus::Done, 2));
    assert_eq!(std::fs::read(out.join("book-p1-1.jpg")).unwrap(), jpeg);
    assert_eq!(std::fs::read(out.join("book-p1-1-2.jpg")).unwrap(), jpeg);
    assert!(out.join("book-p1-2-2.png").is_file());
    assert_eq!(std::fs::read(out.join("book-p1-2.png")).unwrap(), alpha, "the first file is as it was");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 4);
    // Nothing to say about: a folder that is not there, a page that is not there, pages with no pictures.
    engine.export_images(1, 4, &dir.join("missing").to_string_lossy(), "book", vec![0], (72.0, ImageFormat::Png));
    assert!(until(&engine, |e| if let Event::Exported { id: 4, status, .. } = e { Some(status == SaveStatus::Failed) } else { None }));
    engine.export_images(1, 5, &d, "book", vec![9], (72.0, ImageFormat::Png));
    assert!(until(&engine, |e| if let Event::Exported { id: 5, status, .. } = e { Some(status == SaveStatus::Failed) } else { None }));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&out);
    let plain = scratch("no-pictures");
    open(&engine, pdf(1, square));
    engine.extract_images(1, 6, &plain.to_string_lossy(), "none", vec![0]);
    let (status, files) = until(&engine, |e| if let Event::Exported { id: 6, status, pages, .. } = e { Some((status, pages)) } else { None });
    assert_eq!((status, files), (SaveStatus::Done, 0));
    let _ = std::fs::remove_dir_all(&plain);
}

#[test]
fn a_picture_that_would_take_too_much_memory_is_left_out_with_a_word_and_the_rest_is_saved() {
    let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).unwrap();
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /XObject << /A 5 0 R /B 6 0 R >> >> >>");
    b.stream_obj(4, "", b"q 90 0 0 90 5 5 cm /A Do Q q 90 0 0 90 105 5 cm /B Do Q");
    // (Eight thousand pixels a side, with next to no data: it is far over what the reader decodes, and it is not looked at.)
    b.stream_obj(5, "/Type /XObject /Subtype /Image /Width 8000 /Height 8000 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[0; 16]);
    b.stream_obj(6, "/Type /XObject /Subtype /Image /Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", &jpeg);
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, b.finish_classic(10, "/Root 1 0 R"));
    let out = scratch("big-picture");
    engine.extract_images(1, 1, &out.to_string_lossy(), "big", vec![0]);
    let (status, message, files) = until(&engine, |e| if let Event::Exported { id: 1, status, message, pages, .. } = e { Some((status, message, pages)) } else { None });
    // (The JPEG is saved, as it is; the big one is told of, with the page. The pictures of a page are numbered as they are met.)
    assert_eq!((status, files), (SaveStatus::Done, 1), "{message}");
    assert!(message.contains("page 1") && message.contains("8000 by 8000"), "{message}");
    assert_eq!(std::fs::read(out.join("big-p1-2.jpg")).unwrap(), jpeg);
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn the_reader_writes_the_same_bytes_as_the_command_line_does_at_every_resolution() {
    use crate::export::{BAND_PIXELS, write_page_file};
    // A page of 400 by 300 points with a shape and a line of text: at 600 dots to the inch it is 3333 by 2500 pixels, two bands.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"0.2 0.4 0.8 rg 20 20 300 120 re f 1 0 0 RG 6 w 30 200 m 350 280 l S BT /F1 24 Tf 0 g 40 160 Td (Hello bands) Tj ET");
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let data = b.finish_classic(6, "/Root 1 0 R");
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, data.clone());
    assert!(u64::from(o.max_tile_pixels) >= BAND_PIXELS, "the reader's pieces are as big as the bands of the command line");
    let doc = Document::from_bytes(data).unwrap();
    let page = doc.pages().unwrap().remove(0);
    let mut renderer = crate::render::Renderer::new(&doc, crate::forms::painter());
    let dir = scratch("same-bytes");
    let mut id = 0;
    for (dpi, format) in [(150.0, ImageFormat::Png), (600.0, ImageFormat::Png), (600.0, ImageFormat::Jpeg(85)), (72.0, ImageFormat::Jpeg(60))] {
        id += 1;
        let (first, second) = (dir.join(format!("a{id}")), dir.join(format!("b{id}")));
        std::fs::create_dir_all(&first).unwrap();
        engine.export_images(1, id, &first.to_string_lossy(), "x", vec![0], (dpi, format));
        until(&engine, |e| if let Event::Exported { id: i, status: SaveStatus::Done, .. } = e { (i == id).then_some(()) } else { None });
        std::fs::create_dir_all(&second).unwrap();
        let name = crate::export::page_file_name("x", 0, format);
        write_page_file(&mut renderer, &page, dpi, format, BAND_PIXELS, &second.join(&name)).unwrap();
        assert!(std::fs::read(first.join(&name)).unwrap() == std::fs::read(second.join(&name)).unwrap(), "{dpi} dpi, {format:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_long_export_can_be_stopped() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, pdf(40, square));
    let dir = scratch("stop");
    engine.export_images(1, 1, &dir.to_string_lossy(), "p", (0..40).collect(), (150.0, ImageFormat::Png));
    engine.cancel(1);
    // A request that was cancelled gets no answer; the engine goes on with the next.
    engine.properties(1, 2);
    until(&engine, |e| if let Event::Properties { id: 2, .. } = e { Some(()) } else { None });
    assert!(std::fs::read_dir(&dir).unwrap().count() < 40);
    let _ = std::fs::remove_dir_all(&dir);
}

const XMP: &str = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Old</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>";

fn titled_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R /MarkInfo << /Marked true >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 6 0 R >> >> >>");
    b.obj(4, "<< /Title (Old) /Author (A) /Producer (P) /CreationDate (D:20260101080000+08'00') >>");
    b.stream_obj(5, "/Type /Metadata /Subtype /XML", XMP.as_bytes());
    b.obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.finish_classic(10, "/Root 1 0 R /Info 4 0 R")
}

fn properties_of(engine: &Engine, id: u64) -> Box<Properties> {
    engine.properties(1, id);
    until(engine, |e| if let Event::Properties { id: i, props, .. } = e { (i == id).then_some(props) } else { None })
}

#[test]
fn the_properties_are_told_and_changing_them_is_one_step_that_can_be_undone() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, titled_pdf());
    let p = properties_of(&engine, 1);
    assert_eq!((p.version, p.pages), ((1, 4), 1));
    assert_eq!((p.title.as_str(), p.author.as_str(), p.producer.as_str()), ("Old", "A", "P"));
    assert_eq!(p.created.map(|d| (d.utc, d.offset)), Some((1_767_225_600, Some(480))));
    assert!(p.tagged && p.xmp && !p.signed && !p.linearized && p.encryption.is_none());
    assert_eq!((p.attachments, p.scripts, p.fonts.len()), (0, 0, 1));
    assert_eq!((p.fonts[0].name.as_str(), p.fonts[0].embedded), ("Helvetica", Embedded::No));
    // Change two of the four; the others, the dates and the XMP's other parts stay.
    engine.set_properties(1, 2, Meta { title: Some("\u{65B0}\u{6807}\u{9898}".to_string()), keywords: Some("k1, k2".to_string()), ..Meta::default() });
    let r = edited(&engine, 2);
    assert_eq!((r.status, r.page, r.message.as_str(), r.pages.is_none()), (EditStatus::Done, NO_PAGE, "", true));
    assert!(r.dirty && r.can_undo);
    let p = properties_of(&engine, 3);
    assert_eq!((p.title.as_str(), p.author.as_str(), p.keywords.as_str()), ("\u{65B0}\u{6807}\u{9898}", "A", "k1, k2"));
    assert_eq!(p.created.map(|d| d.utc), Some(1_767_225_600));
    // Saved: the original bytes are the start of the file, and the file says the same.
    let dir = scratch("props");
    let path = dir.join("t.pdf");
    engine.save(1, 4, &path.to_string_lossy());
    until(&engine, |e| if let Event::Saved { id: 4, status: SaveStatus::Done, .. } = e { Some(()) } else { None });
    let saved = std::fs::read(&path).unwrap();
    assert!(saved.starts_with(&titled_pdf()));
    let again = Document::from_bytes(saved).unwrap();
    let Object::Stream(x) = again.get(ObjRef::new(5, 0)).unwrap() else { panic!("no XMP") };
    let xml = String::from_utf8(again.decode_stream(&x).unwrap()).unwrap();
    assert!(xml.contains("\u{65B0}\u{6807}\u{9898}") && xml.contains("k1, k2") && !xml.contains(">Old<"), "{xml}");
    let _ = std::fs::remove_dir_all(&dir);
    // Undo: the old title is back.
    engine.undo(1, 5);
    assert_eq!(edited(&engine, 5).status, EditStatus::Done);
    assert_eq!(properties_of(&engine, 6).title, "Old");
}

/// The box opens at once: the properties of a book of 130 pages come back in a tenth of a second (asserted in release builds only; the slow
/// part, the list of fonts, is cut short by its own limits).
#[test]
fn the_properties_of_a_big_book_come_back_quickly() {
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/local/zh/gongwen/gongwen-wuhan-gazette-2024-05.pdf");
    let data = std::fs::read(&corpus).unwrap_or_else(|_| pdf(130, square));
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, data);
    // (The first request is the cold one: nothing of the file has been looked at yet.)
    let begin = std::time::Instant::now();
    let p = properties_of(&engine, 1);
    let took = begin.elapsed();
    eprintln!("properties of {} pages: {took:?}, {} fonts", p.pages, p.fonts.len());
    if !cfg!(debug_assertions) {
        assert!(took < std::time::Duration::from_millis(100), "{took:?}");
    }
}
