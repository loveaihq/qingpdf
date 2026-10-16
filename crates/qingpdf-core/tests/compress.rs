//! Item 3 of the feature list: a file written again smaller (`compress::compress`). Objects that are the same are written once (a font,
//! a picture or a colour profile repeated on every page), the objects are packed into object streams behind a cross-reference stream,
//! Flate data is deflated again, a picture that is drawn at more dots per inch than the preset keeps is made smaller, and the output is
//! never bigger than the input. qpdf checks what we write; the pages must read, draw and say the same. Without qpdf the checks that need
//! it say so and are not made.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use qingpdf_base::testutil::PdfBuilder;
use qingpdf_core::compress::{self, Control, Options, Packed, Preset};
use qingpdf_core::render::Renderer;
use qingpdf_core::security::{Method, Permissions, Protection};
use qingpdf_core::text::TextExtractor;
use qingpdf_core::{Document, Error, Name, ObjRef, Object, ops};

fn ctl() -> Control<'static> {
    Control { stop: None, progress: &|_| {} }
}

fn run(doc: &Document, password: &str, how: &Options) -> Packed {
    compress::compress(doc, password, how, &ctl()).unwrap()
}

fn lossless() -> Options {
    Options { lossless_only: true, ..Options::default() }
}

fn page_texts(doc: &Document) -> Vec<String> {
    let mut extractor = TextExtractor::new(doc);
    doc.pages().unwrap().iter().map(|p| extractor.page_text(p).unwrap()).collect()
}

/// The pages drawn at 72 dpi, as RGBA.
fn drawn(doc: &Document) -> Vec<(u32, u32, Vec<u8>)> {
    let mut r = Renderer::new(doc, qingpdf_core::forms::painter());
    doc.pages().unwrap().iter().map(|p| {
        let b = r.render_page(p, 72.0).unwrap();
        (b.width, b.height, b.rgba)
    }).collect()
}

/// The biggest difference in one sample, and the mean difference, between two renderings of the same pages.
fn differ(a: &[(u32, u32, Vec<u8>)], b: &[(u32, u32, Vec<u8>)]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let (mut most, mut sum, mut n) = (0u8, 0u64, 0u64);
    for (x, y) in a.iter().zip(b) {
        assert_eq!((x.0, x.1), (y.0, y.1));
        for (p, q) in x.2.iter().zip(&y.2) {
            let d = p.abs_diff(*q);
            most = most.max(d);
            sum += u64::from(d);
            n += 1;
        }
    }
    (most, sum as f64 / n.max(1) as f64)
}

/// `pages` pages, each with a font, a graphics state, a colour space and a 40 by 40 grey picture of its own copies of the same things,
/// and its own text. Object numbers: 1 catalog, 2 pages, then six for each page.
fn repeated(pages: u32) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + 6 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    let grey: Vec<u8> = (0..1600).map(|i| ((i % 40) * 6) as u8).collect();
    for i in 0..pages {
        let n = 3 + 6 * i;
        b.obj(n, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R /Resources << /Font << /F1 {} 0 R >> /ExtGState << /G1 {} 0 R >> /XObject << /Im1 {} 0 R >> >> >>", n + 1, n + 2, n + 3, n + 4));
        let content = format!("BT /F1 12 Tf 20 150 Td (Page number {}) Tj ET\nq /G1 gs 80 0 0 80 20 20 cm /Im1 Do Q", i + 1);
        b.stream_obj(n + 1, "", content.as_bytes());
        b.obj(n + 2, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
        b.obj(n + 3, "<< /Type /ExtGState /CA 0.9 /ca 0.9 >>");
        b.stream_obj(n + 4, "/Type /XObject /Subtype /Image /Width 40 /Height 40 /ColorSpace /DeviceGray /BitsPerComponent 8", &grey);
        b.obj(n + 5, "[1 2 3]");
    }
    b.finish_classic(3 + 6 * pages, "/Root 1 0 R")
}

/// Samples of a smooth picture with a little noise (a fixed generator): what a photograph is like to a coder, which a repeating pattern is not.
fn smooth(w: usize, h: usize, comps: usize) -> Vec<u8> {
    let mut seed = 99u32;
    let mut out = Vec::with_capacity(w * h * comps);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            for c in 0..comps {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = f64::from(seed >> 24) / 255.0 * 6.0;
                out.push((128.0 + 100.0 * (fx * (4.0 + c as f64)).sin() * (fy * 3.0).cos() + 20.0 * fx + noise) as u8);
            }
        }
    }
    out
}

fn count_where(doc: &Document, test: impl Fn(&Object) -> bool) -> usize {
    (1..doc.next_free_number()).filter(|&n| doc.get(ObjRef::new(n, 0)).is_ok_and(|o| test(&o))).count()
}

fn is_image(o: &Object) -> bool {
    matches!(o, Object::Stream(s) if s.dict.get_name("Subtype").is_some_and(|n| n == "Image"))
}

#[test]
fn objects_that_repeat_are_written_once_and_the_pages_read_and_draw_the_same() {
    let bytes = repeated(6);
    let doc = Document::from_bytes(bytes.clone()).unwrap();
    let packed = run(&doc, "", &lossless());
    let out = packed.data.expect("the file got smaller");
    assert!(out.len() < bytes.len() / 2, "{} of {}", out.len(), bytes.len());
    assert_eq!(packed.before, bytes.len() as u64);
    assert_eq!(packed.after, out.len() as u64);
    let again = Document::from_bytes(out).unwrap();
    assert!(!again.was_repaired() && again.uses_xref_streams() && again.uses_object_streams());
    // One picture, one font and one graphics state for all six pages.
    assert_eq!(count_where(&again, is_image), 1);
    assert_eq!(count_where(&again, |o| o.as_dict().is_some_and(|d| d.get_name("Type").is_some_and(|t| t == "Font"))), 1);
    assert_eq!(count_where(&again, |o| o.as_dict().is_some_and(|d| d.get_name("Type").is_some_and(|t| t == "ExtGState"))), 1);
    assert!(packed.merged >= 15, "{} merged", packed.merged);
    assert_eq!(page_texts(&again), page_texts(&doc));
    assert_eq!(page_texts(&again)[2].trim(), "Page number 3");
    assert_eq!(drawn(&again), drawn(&doc), "the pages draw to the same pixels");
    // The pages are still six different pages (they were not merged), and the picture is in all of them.
    assert_eq!(again.page_count().unwrap(), 6);
    for p in again.pages().unwrap() {
        let res = again.resolve(p.resources().unwrap()).unwrap();
        assert!(res.as_dict().unwrap().contains_key("XObject"));
    }
}

#[test]
fn a_file_that_cannot_get_smaller_is_kept_as_it_is() {
    let doc = Document::from_bytes(PdfBuilder::new().finish_classic(1, "")).ok();
    // A file with nothing in it is not one to compress; the smallest real one is.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
    let bytes = b.finish_classic(4, "/Root 1 0 R");
    let _ = doc;
    let doc = Document::from_bytes(bytes.clone()).unwrap();
    let packed = run(&doc, "", &Options::default());
    assert!(packed.data.is_none() && packed.after == packed.before && packed.before == bytes.len() as u64);
}

#[test]
fn a_picture_drawn_at_more_dots_per_inch_than_the_preset_keeps_is_made_smaller_and_looks_the_same() {
    // A 600 by 600 colour picture (smooth, as a photograph is), drawn 100 points square: 432 dpi.
    let raw = smooth(600, 600, 3);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"q 100 0 0 100 50 50 cm /Im1 Do Q");
    b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 600 /Height 600 /ColorSpace /DeviceRGB /BitsPerComponent 8", &raw);
    let bytes = b.finish_classic(6, "/Root 1 0 R");
    let doc = Document::from_bytes(bytes.clone()).unwrap();
    let before = drawn(&doc);
    for (preset, dpi) in [(Preset::High, 200u32), (Preset::Balanced, 150), (Preset::Small, 100)] {
        let packed = run(&doc, "", &Options { preset, ..Options::default() });
        let out = packed.data.expect("smaller");
        assert!(out.len() < bytes.len() / 5, "{preset:?}: {} of {}", out.len(), bytes.len());
        assert_eq!(packed.pictures, 1);
        let again = Document::from_bytes(out).unwrap();
        let Object::Stream(s) = again.get(find_image(&again)).unwrap() else { panic!("not a stream") };
        let want = (100.0 * f64::from(dpi) / 72.0).ceil() as i64;
        assert_eq!(s.dict.get_int("Width"), Some(want), "{preset:?}");
        assert_eq!(s.dict.get_int("Height"), Some(want));
        assert_eq!(s.dict.get_name("Filter").unwrap(), &Name::from("DCTDecode"));
        let (most, mean) = differ(&before, &drawn(&again));
        assert!(mean < 2.5, "{preset:?}: mean difference {mean} (most {most})");
    }
    // Only what loses nothing: the picture is as it was (deflated again, if that is smaller).
    let packed = run(&doc, "", &lossless());
    let kept = Document::from_bytes(packed.data.unwrap_or(bytes.clone())).unwrap();
    let Object::Stream(s) = kept.get(find_image(&kept)).unwrap() else { panic!() };
    assert_eq!(s.dict.get_int("Width"), Some(600));
    assert_eq!(kept.decode_stream(&s).unwrap(), raw);
    assert_eq!(drawn(&kept), before);
}

fn find_image(doc: &Document) -> ObjRef {
    (1..doc.next_free_number()).map(|n| ObjRef::new(n, 0)).find(|r| doc.get(*r).is_ok_and(|o| is_image(&o))).expect("no picture")
}

/// A page that draws the picture `/Im1` (a 400 by 400 grey gradient) in the way `how` says, and the extra objects.
fn picture_page(image_entries: &str, extra_objects: &[(u32, &str)], content: &str, resources_extra: &str) -> Vec<u8> {
    let raw = smooth(400, 400, 1);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> {resources_extra} >> >>"));
    b.stream_obj(4, "", content.as_bytes());
    b.flate_stream_obj(5, &format!("/Type /XObject /Subtype /Image /Width 400 /Height 400 {image_entries}"), &raw);
    let mut size = 6;
    for (n, body) in extra_objects {
        b.obj(*n, body);
        size = size.max(n + 1);
    }
    b.finish_classic(size, "/Root 1 0 R")
}

fn picture_size(bytes: Vec<u8>, how: &Options) -> (i64, Option<Vec<u8>>) {
    let doc = Document::from_bytes(bytes).unwrap();
    let packed = run(&doc, "", how);
    match packed.data {
        None => (400, None),
        Some(out) => {
            let again = Document::from_bytes(out.clone()).unwrap();
            let Object::Stream(s) = again.get(find_image(&again)).unwrap() else { panic!() };
            (s.dict.get_int("Width").unwrap(), Some(out))
        }
    }
}

#[test]
fn what_cannot_be_changed_exactly_is_left_as_it_is() {
    let draw = "q 100 0 0 100 50 50 cm /Im1 Do Q";
    let grey = "/ColorSpace /DeviceGray /BitsPerComponent 8";
    let all = Options::default();
    // The same picture, drawn small, is made smaller...
    assert!(picture_size(picture_page(grey, &[], draw, ""), &all).0 < 400);
    // ... and is not when it is a stencil mask, 1 bit, has a /Decode, a colour key mask, 16 bits, an Indexed or a CMYK colour space.
    for entries in [
        "/ImageMask true /BitsPerComponent 1",
        "/ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]",
        "/ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [0 10]",
        "/ColorSpace /DeviceGray /BitsPerComponent 16",
        "/ColorSpace [/Indexed /DeviceRGB 255 <000000>] /BitsPerComponent 8",
        "/ColorSpace /DeviceCMYK /BitsPerComponent 8",
        "/ColorSpace /DeviceGray /BitsPerComponent 4",
        "/ColorSpace /DeviceGray /BitsPerComponent 8 /SMaskInData 1",
    ] {
        let (width, _) = picture_size(picture_page(entries, &[], draw, ""), &all);
        assert_eq!(width, 400, "{entries}");
    }
    // A picture that a tiling pattern, a Type 3 glyph or a soft mask group draws is left as it is, drawn small or not.
    let pattern = picture_page(
        grey,
        &[(6, "<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 100 100] /XStep 100 /YStep 100 /Resources << /XObject << /Im1 5 0 R >> >> /Length 33 >>")],
        "/Pattern cs /P1 scn 0 0 200 200 re f",
        "/Pattern << /P1 6 0 R >>",
    );
    // (A pattern is a stream; make it one by hand.)
    let _ = pattern;
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Pattern << /P1 6 0 R >> /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"/Pattern cs /P1 scn 0 0 200 200 re f q 100 0 0 100 50 50 cm /Im1 Do Q");
    let raw = smooth(400, 400, 1);
    b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 8", &raw);
    b.stream_obj(6, "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 100 100] /XStep 100 /YStep 100 /Resources << /XObject << /Im1 5 0 R >> >>", b"q 50 0 0 50 0 0 cm /Im1 Do Q");
    let (width, _) = picture_size(b.finish_classic(7, "/Root 1 0 R"), &all);
    assert_eq!(width, 400, "a picture a pattern draws");
    // Type 3 glyph.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"BT /F1 20 Tf 20 100 Td (a) Tj ET q 100 0 0 100 50 50 cm /Im1 Do Q");
    b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 8", &raw);
    b.obj(6, "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /a 7 0 R >> /Encoding << /Type /Encoding /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [1] /Resources << /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(7, "", b"1 0 0 0 1 1 d1 q 1 0 0 1 0 0 cm /Im1 Do Q");
    let (width, _) = picture_size(b.finish_classic(8, "/Root 1 0 R"), &all);
    assert_eq!(width, 400, "a picture a Type 3 glyph draws");
}

#[test]
fn a_soft_mask_follows_its_picture_and_a_picture_drawn_big_stays_big() {
    let mask = smooth(400, 400, 1);
    let build = |size: &str| {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 500 500] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>");
        b.stream_obj(4, "", format!("q {size} 0 0 {size} 10 10 cm /Im1 Do Q").as_bytes());
        let raw = smooth(400, 400, 3);
        b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R", &raw);
        b.flate_stream_obj(6, "/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 8", &mask);
        b.finish_classic(7, "/Root 1 0 R")
    };
    // Drawn 100 points wide: both go down to 209 pixels (150 dpi), the mask in Flate.
    let doc = Document::from_bytes(build("100")).unwrap();
    let out = run(&doc, "", &Options::default()).data.expect("smaller");
    let again = Document::from_bytes(out).unwrap();
    let Object::Stream(image) = again.get(find_image(&again)).unwrap() else { panic!() };
    assert_eq!(image.dict.get_int("Width"), Some(209));
    let Some(Object::Ref(m)) = image.dict.get("SMask") else { panic!("the mask is gone") };
    let Object::Stream(mask_out) = again.get(*m).unwrap() else { panic!() };
    assert_eq!((mask_out.dict.get_int("Width"), mask_out.dict.get_int("Height")), (Some(209), Some(209)));
    assert_eq!(mask_out.dict.get_name("Filter").unwrap(), &Name::from("FlateDecode"));
    assert_eq!(again.decode_stream(&mask_out).unwrap().len(), 209 * 209);
    // Drawn 400 points wide (72 dpi on the page, 72 pixels to the inch of a 400 pixel picture): nothing to give up in size; the picture is
    // not made smaller in pixels.
    let doc = Document::from_bytes(build("400")).unwrap();
    let out = run(&doc, "", &Options::default());
    if let Some(data) = out.data {
        let again = Document::from_bytes(data).unwrap();
        let Object::Stream(image) = again.get(find_image(&again)).unwrap() else { panic!() };
        assert_eq!(image.dict.get_int("Width"), Some(400));
    }
}

#[test]
fn the_output_is_checked_by_qpdf_for_the_files_of_the_corpus() {
    let Some(qpdf) = common::find_qpdf() else {
        eprintln!("qpdf is not here: the output is not checked");
        return;
    };
    let dir = common::fresh_out_dir("compress-qpdf");
    let files = [
        "zh/gongwen/gongwen-2025-service-mfg-plan.pdf",
        "zh/gongwen/gongwen-2024-nicheng-work-report.pdf",
        "zh/gongwen/gongwen-2024-ai-standards-guide.pdf",
        "xref-stream-objstm/bug_757705.pdf",
    ];
    for rel in files {
        let path = common::public_file(rel);
        let doc = Document::open(&path).unwrap();
        for how in [lossless(), Options::default(), Options { preset: Preset::Small, ..Options::default() }] {
            let packed = run(&doc, "", &how);
            let Some(data) = packed.data else { continue };
            let out = dir.join("out.pdf");
            std::fs::write(&out, &data).unwrap();
            let verdict = common::qpdf_check(&qpdf, &out);
            assert_eq!(verdict.code, 0, "{rel} {how:?}: {}", verdict.text);
            let again = Document::from_bytes(data).unwrap();
            assert_eq!(page_texts(&again), page_texts(&doc), "{rel} {how:?}");
        }
    }
}

#[test]
fn an_encrypted_file_stays_encrypted_the_same_way_with_the_same_passwords() {
    let path = common::public_file("zh/gongwen/gongwen-2024-ai-standards-guide.pdf");
    let doc = Document::open(&path).unwrap();
    let all = Permissions { print: true, print_high_quality: true, modify: true, copy: true, annotate: true, fill_forms: true, assemble: true, accessibility: true };
    for method in [Method::AesV3, Method::AesV2] {
        let how = Protection { method, user_password: "open sesame".to_string(), owner_password: "owner pw".to_string(), permissions: all, encrypt_metadata: true };
        let encrypted = ops::protect(&doc, "", &how, &[7u8; 32]).unwrap();
        let locked = Document::from_bytes(encrypted.data.clone()).unwrap();
        assert!(locked.is_locked());
        let opened = Document::from_bytes_with_password(encrypted.data.clone(), "open sesame").unwrap();
        for how in [lossless(), Options::default()] {
            let packed = run(&opened, "open sesame", &how);
            let data = packed.data.expect("smaller");
            assert!(data.len() < encrypted.data.len());
            // Same method, same passwords: the user password opens it, the owner password opens it, a wrong one does not.
            assert!(Document::from_bytes(data.clone()).unwrap().is_locked());
            assert!(matches!(Document::from_bytes_with_password(data.clone(), "wrong"), Err(Error::WrongPassword)));
            for pw in ["open sesame", "owner pw"] {
                let again = Document::from_bytes_with_password(data.clone(), pw).unwrap();
                assert!(!again.is_locked());
                assert_eq!(page_texts(&again), page_texts(&doc), "{pw}");
                let e = again.encryption().unwrap();
                assert_eq!(e.method_name(), opened.encryption().unwrap().method_name());
            }
            if let Some(qpdf) = common::find_qpdf() {
                let dir = common::fresh_out_dir("compress-qpdf-encrypted");
                let out = dir.join("out.pdf");
                std::fs::write(&out, &data).unwrap();
                let verdict = common::qpdf_check_pw(&qpdf, &out, "open sesame");
                assert_eq!(verdict.code, 0, "{method:?}: {}", verdict.text);
            }
        }
    }
}

#[test]
fn a_signed_file_is_refused_and_a_file_that_may_not_be_changed_needs_its_owner_password() {
    let signed = common::public_file("incremental/signature_reason.pdf");
    let doc = Document::open(&signed).unwrap();
    let e = compress::compress(&doc, "", &Options::default(), &ctl()).unwrap_err();
    assert!(e.to_string().contains("signature"), "{e}");
    // A file whose author did not allow changing the content: the user password is not enough, the owner password is.
    let src = Document::open(common::public_file("zh/gongwen/gongwen-2025-service-mfg-plan.pdf")).unwrap();
    let mut rights = Permissions { print: true, print_high_quality: true, modify: false, copy: true, annotate: true, fill_forms: true, assemble: true, accessibility: true };
    let how = Protection { method: Method::AesV3, user_password: "u".to_string(), owner_password: "o".to_string(), permissions: rights, encrypt_metadata: true };
    let data = ops::protect(&src, "", &how, &[9u8; 32]).unwrap().data;
    let as_user = Document::from_bytes_with_password(data.clone(), "u").unwrap();
    let e = compress::compress(&as_user, "u", &Options::default(), &ctl()).unwrap_err();
    assert!(e.to_string().contains("did not allow changing its content"), "{e}");
    // ... but what loses nothing is a plain rewrite, like rotating or taking pages out: the user password is enough, the protection stays.
    let packed = compress::compress(&as_user, "u", &lossless(), &ctl()).unwrap();
    if let Some(out) = packed.data {
        assert!(Document::from_bytes(out.clone()).unwrap().is_locked() && Document::from_bytes_with_password(out.clone(), "u").is_ok());
        assert!(matches!(Document::from_bytes_with_password(out, "wrong"), Err(Error::WrongPassword)));
    }
    let as_owner = Document::from_bytes_with_password(data, "o").unwrap();
    assert!(compress::compress(&as_owner, "o", &Options::default(), &ctl()).is_ok());
    // (A file that allows it: no owner password needed.)
    rights.modify = true;
    let how = Protection { method: Method::AesV3, user_password: "u".to_string(), owner_password: "o".to_string(), permissions: rights, encrypt_metadata: true };
    let data = ops::protect(&src, "", &how, &[9u8; 32]).unwrap().data;
    let as_user = Document::from_bytes_with_password(data, "u").unwrap();
    assert!(compress::compress(&as_user, "u", &Options::default(), &ctl()).is_ok());
}

#[test]
fn work_that_is_cancelled_ends_in_cancelled_and_reports_how_far_it_is() {
    let doc = Document::from_bytes(repeated(30)).unwrap();
    let stop = Arc::new(AtomicBool::new(true));
    let control = Control { stop: Some(stop), progress: &|_| {} };
    assert!(matches!(compress::compress(&doc, "", &Options::default(), &control), Err(Error::Cancelled)));
    // The progress goes up, from 0 to 100, and never back.
    let seen = std::cell::RefCell::new(Vec::new());
    let progress = |p: u32| seen.borrow_mut().push(p);
    compress::compress(&doc, "", &Options::default(), &Control { stop: None, progress: &progress }).unwrap();
    let seen = seen.into_inner();
    assert_eq!((seen.first(), seen.last()), (Some(&0), Some(&100)));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    let distinct: HashSet<u32> = seen.into_iter().collect();
    assert!(distinct.len() >= 3);
}

#[test]
fn the_same_input_gives_the_same_output() {
    let doc = Document::from_bytes(repeated(5)).unwrap();
    let a = run(&doc, "", &Options::default()).data.unwrap();
    let b = run(&doc, "", &Options::default()).data.unwrap();
    assert_eq!(a, b);
    // And compressing what was compressed does not make it bigger.
    let again = Document::from_bytes(a.clone()).unwrap();
    let c = run(&again, "", &Options::default());
    assert!(c.after <= a.len() as u64);
}

#[test]
fn a_page_with_a_thumbnail_and_private_data_loses_them_and_keeps_the_document_metadata() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R /PieceInfo << /X 1 >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Thumb 5 0 R /PieceInfo << /Y 2 >> /Contents 4 0 R /Metadata 7 0 R >>");
    b.stream_obj(4, "", b"0 0 m 10 10 l S");
    b.stream_obj(5, "/Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceGray /BitsPerComponent 8", &vec![5u8; 4096]);
    b.stream_obj(6, "/Type /Metadata /Subtype /XML", b"<x:xmpmeta>document</x:xmpmeta>");
    b.stream_obj(7, "/Type /Metadata /Subtype /XML", &b"<x:xmpmeta>page of the file, which is not read by anything</x:xmpmeta>".repeat(8));
    let doc = Document::from_bytes(b.finish_classic(8, "/Root 1 0 R")).unwrap();
    let out = run(&doc, "", &lossless()).data.expect("smaller");
    let again = Document::from_bytes(out).unwrap();
    let cat = again.catalog().unwrap();
    assert!(cat.contains_key("Metadata") && !cat.contains_key("PieceInfo"));
    let page = again.pages().unwrap().remove(0);
    assert!(!page.dict.contains_key("Thumb") && !page.dict.contains_key("PieceInfo") && !page.dict.contains_key("Metadata"));
    assert_eq!(count_where(&again, is_image), 0, "the thumbnail is gone");
}

// --- through the reader's engine (File > Compress) ---------------------------------------------------------------------

use qingpdf_core::view::{CompressHow, Event, SaveStatus};

/// What the engine says to [`Engine::compress`](qingpdf_core::view::Engine::compress) with the number `id`: the progress it told, and the answer.
fn compressed(v: &common::Viewer, id: u64) -> (Vec<u32>, (SaveStatus, String, u64, u64, bool)) {
    let mut progress = Vec::new();
    let answer = v.until(|e| match e {
        Event::CompressProgress { id: i, percent, .. } if i == id => {
            progress.push(percent);
            None
        }
        Event::Compressed { id: i, status, message, before, after, kept, .. } if i == id => Some((status, message, before, after, kept)),
        _ => None,
    });
    (progress, answer)
}

#[test]
fn the_engine_compresses_the_document_with_its_unsaved_edits_and_tells_how_far_it_is() {
    let dir = common::fresh_out_dir("compress-engine");
    let original = dir.join("flyer.pdf");
    std::fs::copy(common::public_file("zh/gongwen/gongwen-2024-ai-standards-guide.pdf"), &original).unwrap();
    let before_bytes = std::fs::read(&original).unwrap();
    let mut v = common::Viewer::open(&original, "");
    // An edit that is not saved: the first page is turned. It must be in the result.
    let id = v.id();
    v.engine.rotate_pages(1, id, vec![0], 90);
    assert_eq!(v.edited(id).status, qingpdf_core::view::EditStatus::Done);
    let target = dir.join("small.pdf");
    let id = v.id();
    v.engine.compress(1, id, &target.to_string_lossy(), CompressHow { preset: Preset::Small, lossless_only: false });
    let (progress, (status, message, before, after, kept)) = compressed(&v, id);
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert!(!kept && before == before_bytes.len() as u64 && after == std::fs::metadata(&target).unwrap().len() && after < before / 2, "{before} -> {after}");
    assert!(progress.windows(2).all(|w| w[0] <= w[1]) && progress.iter().all(|&p| p <= 100) && progress.len() >= 2, "{progress:?}");
    // The open file was not touched; the new one has the turned page and opens, and the pictures are smaller.
    assert_eq!(std::fs::read(&original).unwrap(), before_bytes);
    let doc = Document::open(&target).unwrap();
    assert_eq!(doc.pages().unwrap()[0].rotate(), 90, "the edit that was not saved is in the file");
    assert_eq!(doc.page_count().unwrap(), 13);
    // Only what loses nothing, over the file that is open: a whole rewrite; the old bytes are replaced.
    let id = v.id();
    v.engine.compress(1, id, &original.to_string_lossy(), CompressHow { preset: Preset::Balanced, lossless_only: true });
    let (_, (status, message, ..)) = compressed(&v, id);
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert!(Document::open(&original).unwrap().uses_object_streams());
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn the_engine_refuses_a_signed_file_and_one_that_may_not_be_changed_and_says_when_nothing_is_smaller() {
    let dir = common::fresh_out_dir("compress-engine-refusals");
    let target = dir.join("out.pdf");
    let ask = |file: &std::path::Path, password: &str| {
        let mut v = common::Viewer::open(file, password);
        let id = v.id();
        v.engine.compress(1, id, &target.to_string_lossy(), CompressHow { preset: Preset::Balanced, lossless_only: false });
        compressed(&v, id).1
    };
    let (status, message, ..) = ask(&common::public_file("incremental/signature_reason.pdf"), "");
    assert!(status == SaveStatus::Failed && message.contains("signature") && !target.exists(), "{message}");
    // The user password of a file that does not allow changing the content (the generated files of the encryption tests).
    let (status, message, ..) = ask(&common::generated_dir().join("bookmarks.r3-128-rc4-user-modify-none.pdf"), "user");
    assert!(status == SaveStatus::Failed && message.contains("did not allow changing its content") && !target.exists(), "{message}");
    // ... and with only what loses nothing it is allowed for the user password too.
    let mut v = common::Viewer::open(&common::generated_dir().join("bookmarks.r3-128-rc4-user-modify-none.pdf"), "user");
    let id = v.id();
    v.engine.compress(1, id, &target.to_string_lossy(), CompressHow { preset: Preset::Balanced, lossless_only: true });
    let (status, message, ..) = compressed(&v, id).1;
    assert_eq!(status, SaveStatus::Done, "{message}");
    let _ = std::fs::remove_file(&target);
    // The owner password: allowed (the answer is that nothing got smaller, for a file of 2 KB, or the new file).
    let (status, message, before, after, kept) = ask(&common::generated_dir().join("bookmarks.r3-128-rc4-user-modify-none.pdf"), "owner");
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert!(before > 0 && after <= before && (kept == !target.exists()));
    let _ = std::fs::remove_file(&target);
    // A file that cannot get smaller: Done, kept, no file.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
    let tiny = dir.join("tiny.pdf");
    std::fs::write(&tiny, b.finish_classic(4, "/Root 1 0 R")).unwrap();
    let (status, _, before, after, kept) = ask(&tiny, "");
    assert!(status == SaveStatus::Done && kept && before == after && !target.exists());
}

#[test]
fn a_compression_that_is_cancelled_gives_no_file_or_a_whole_one() {
    let dir = common::fresh_out_dir("compress-engine-cancel");
    let original = dir.join("flyer.pdf");
    std::fs::copy(common::public_file("zh/gongwen/gongwen-2024-ai-standards-guide.pdf"), &original).unwrap();
    let mut v = common::Viewer::open(&original, "");
    let target = dir.join("out.pdf");
    let id = v.id();
    v.engine.compress(1, id, &target.to_string_lossy(), CompressHow { preset: Preset::Small, lossless_only: false });
    v.engine.cancel(id);
    // Either it was stopped (no answer), or it was done before the cancel came (the answer, and then a whole file).
    let mut answered = false;
    for _ in 0..100 {
        match v.engine.wait(50) {
            Some(Event::Compressed { id: i, status, .. }) if i == id => {
                assert_eq!(status, SaveStatus::Done);
                answered = true;
                break;
            }
            _ => {}
        }
    }
    if answered {
        assert!(Document::open(&target).is_ok());
    } else {
        assert!(!target.exists(), "a file was written for a request that was cancelled");
    }
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_picture_that_a_pattern_draws_on_a_page_with_no_xobject_of_its_own_is_left_as_it_is_whatever_another_page_does() {
    let raw = smooth(400, 400, 1);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>");
    // Page 1 names no XObject: only a pattern, whose own resources hold the picture.
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Pattern << /P1 6 0 R >> >> >>");
    b.stream_obj(4, "", b"/Pattern cs /P1 scn 0 0 200 200 re f");
    b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceGray /BitsPerComponent 8", &raw);
    b.stream_obj(6, "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 100 100] /XStep 100 /YStep 100 /Resources << /XObject << /Im1 5 0 R >> >>", b"q 100 0 0 100 0 0 cm /Im1 Do Q");
    // Page 2 draws the same picture small.
    b.obj(7, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 8 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(8, "", b"q 100 0 0 100 50 50 cm /Im1 Do Q");
    let doc = Document::from_bytes(b.finish_classic(9, "/Root 1 0 R")).unwrap();
    let packed = run(&doc, "", &Options::default());
    assert_eq!(packed.pictures, 0, "the picture is used where its size cannot be measured");
    if let Some(data) = packed.data {
        let again = Document::from_bytes(data).unwrap();
        let Object::Stream(s) = again.get(find_image(&again)).unwrap() else { panic!() };
        assert_eq!(s.dict.get_int("Width"), Some(400));
    }
}

/// A page that draws the form `/Fm1` (an image of 1000 by 1000 smooth colour samples, drawn 10,000 units wide in the form) in the ways
/// `content` says.
fn form_page(content: &str) -> Vec<u8> {
    let raw = smooth(1000, 1000, 3);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 700 700] /Contents 4 0 R /Resources << /XObject << /Fm1 6 0 R >> >> >>");
    b.stream_obj(4, "", content.as_bytes());
    b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 1000 /Height 1000 /ColorSpace /DeviceRGB /BitsPerComponent 8", &raw);
    b.stream_obj(6, "/Type /XObject /Subtype /Form /BBox [0 0 10000 10000] /Resources << /XObject << /Im1 5 0 R >> >>", b"q 10000 0 0 10000 0 0 cm /Im1 Do Q");
    b.finish_classic(7, "/Root 1 0 R")
}

#[test]
fn a_form_drawn_small_and_then_bigger_gives_its_picture_the_size_of_the_bigger_draw() {
    let all = Options::default();
    // 100 points wide (720 dots per inch) and then 500 points wide (144 dots per inch, less than the preset keeps): left as it is.
    let both = "q 0.01 0 0 0.01 20 20 cm /Fm1 Do Q q 0.05 0 0 0.05 100 100 cm /Fm1 Do Q";
    let (width, _) = picture_size_of(form_page(both), &all);
    assert_eq!(width, 1000, "sized for the small draw");
    // Only the small one: made smaller to the size of that draw (about 209 pixels).
    let (width, out) = picture_size_of(form_page("q 0.01 0 0 0.01 20 20 cm /Fm1 Do Q"), &all);
    assert!(out.is_some() && (150..=260).contains(&width), "{width}");
    // The other way round too.
    let (width, _) = picture_size_of(form_page("q 0.05 0 0 0.05 100 100 cm /Fm1 Do Q q 0.01 0 0 0.01 20 20 cm /Fm1 Do Q"), &all);
    assert_eq!(width, 1000);
}

/// Like `picture_size`, for the picture of any size: the width it ends with, and the output if there is one.
fn picture_size_of(bytes: Vec<u8>, how: &Options) -> (i64, Option<Vec<u8>>) {
    let doc = Document::from_bytes(bytes).unwrap();
    let Object::Stream(before) = doc.get(find_image(&doc)).unwrap() else { panic!() };
    let width = before.dict.get_int("Width").unwrap();
    match run(&doc, "", how).data {
        None => (width, None),
        Some(out) => {
            let again = Document::from_bytes(out.clone()).unwrap();
            let Object::Stream(s) = again.get(find_image(&again)).unwrap() else { panic!() };
            (s.dict.get_int("Width").unwrap(), Some(out))
        }
    }
}

/// A page that draws a picture of `dict_side` by `dict_side` (as the dictionary says), coded as the JPEG `jpeg`, 100 points wide.
fn jpeg_page(dict_side: usize, comps: usize, extra: &str, jpeg: &[u8]) -> Vec<u8> {
    let space = if comps == 1 { "DeviceGray" } else { "DeviceRGB" };
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"q 100 0 0 100 50 50 cm /Im1 Do Q");
    b.stream_obj(5, &format!("/Type /XObject /Subtype /Image /Width {dict_side} /Height {dict_side} /ColorSpace /{space} /BitsPerComponent 8 /Filter /DCTDecode {extra}"), jpeg);
    b.finish_classic(6, "/Root 1 0 R")
}

#[test]
fn a_jpeg_that_is_not_the_size_the_dictionary_says_is_left_as_it_is_and_one_that_is_is_made_smaller() {
    use qingpdf_core::squeeze::jpeg::{Raster, encode};
    let all = Options::default();
    let samples = smooth(1200, 1200, 3);
    let big = encode(&Raster { w: 1200, h: 1200, comps: 3, data: &samples }, 90).unwrap();
    // The control: the dictionary says 1200 and the JPEG is 1200 (864 dots per inch): made smaller.
    let doc = Document::from_bytes(jpeg_page(1200, 3, "", &big)).unwrap();
    assert_eq!(run(&doc, "", &all).pictures, 1);
    // The dictionary says 100 by 100, the JPEG says 1200 by 1200: not decoded, left as it is, the rest of the file still compressed.
    let doc = Document::from_bytes(jpeg_page(100, 3, "", &big)).unwrap();
    let packed = run(&doc, "", &all);
    assert_eq!(packed.pictures, 0);
    if let Some(out) = packed.data {
        let again = Document::from_bytes(out).unwrap();
        let Object::Stream(s) = again.get(find_image(&again)).unwrap() else { panic!() };
        assert!(s.data == big && s.dict.get_int("Width") == Some(100));
    }
    // The other way round: a small JPEG in a big dictionary.
    let small_samples = smooth(100, 100, 3);
    let small = encode(&Raster { w: 100, h: 100, comps: 3, data: &small_samples }, 90).unwrap();
    let doc = Document::from_bytes(jpeg_page(1200, 3, "", &small)).unwrap();
    assert_eq!(run(&doc, "", &all).pictures, 0);
}

#[test]
fn a_jpeg_whose_colour_transform_is_off_is_left_as_it_is_and_one_that_says_yes_is_made_smaller() {
    use qingpdf_core::squeeze::jpeg::{Raster, encode};
    let all = Options::default();
    let samples = smooth(900, 900, 3);
    let jpeg = encode(&Raster { w: 900, h: 900, comps: 3, data: &samples }, 90).unwrap();
    let pictures = |extra: &str| run(&Document::from_bytes(jpeg_page(900, 3, extra, &jpeg)).unwrap(), "", &all).pictures;
    assert_eq!(pictures(""), 1);
    assert_eq!(pictures("/DecodeParms << /ColorTransform 1 >>"), 1);
    assert_eq!(pictures("/DecodeParms << /ColorTransform 0 >>"), 0);
    assert_eq!(pictures("/DecodeParms [ << /ColorTransform 0 >> ]"), 0);
    // One component has no colour transform to get wrong.
    let grey = smooth(900, 900, 1);
    let jpeg = encode(&Raster { w: 900, h: 900, comps: 1, data: &grey }, 90).unwrap();
    assert_eq!(run(&Document::from_bytes(jpeg_page(900, 1, "/DecodeParms << /ColorTransform 0 >>", &jpeg)).unwrap(), "", &all).pictures, 1);
}

/// A 400 by 400 colour picture drawn 100 points wide, with a soft mask of `mask_side` by `mask_side` flat grey samples.
fn masked_page(mask_side: usize) -> Vec<u8> {
    let raw = smooth(400, 400, 3);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>");
    b.stream_obj(4, "", b"q 100 0 0 100 50 50 cm /Im1 Do Q");
    b.flate_stream_obj(5, "/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R", &raw);
    b.flate_stream_obj(6, &format!("/Type /XObject /Subtype /Image /Width {mask_side} /Height {mask_side} /ColorSpace /DeviceGray /BitsPerComponent 8"), &vec![200u8; mask_side * mask_side]);
    b.finish_classic(7, "/Root 1 0 R")
}

/// The widths of the pictures of a document, the smallest first.
fn widths(out: Vec<u8>) -> Vec<i64> {
    let doc = Document::from_bytes(out).unwrap();
    let mut all: Vec<i64> = (1..doc.next_free_number())
        .filter_map(|n| match doc.get(ObjRef::new(n, 0)) {
            Ok(Object::Stream(s)) if s.dict.get_name("Subtype").is_some_and(|n| n == "Image") => s.dict.get_int("Width"),
            _ => None,
        })
        .collect();
    all.sort_unstable();
    all
}

#[test]
fn a_soft_mask_that_is_bigger_than_the_memory_lent_is_left_and_the_picture_is_still_made_smaller() {
    let bytes = masked_page(5000);
    // 25 million mask samples do not fit 16 MiB: the picture is made smaller (a JPEG, about 209 wide), the mask stays 5000 wide.
    let tight = Options { picture_memory: 16 << 20, ..Options::default() };
    let packed = run(&Document::from_bytes(bytes.clone()).unwrap(), "", &tight);
    assert_eq!(packed.pictures, 1);
    let w = widths(packed.data.expect("smaller"));
    assert!(w[0] < 300 && w[1] == 5000, "{w:?}");
    // With the memory for it, the mask follows the picture.
    let roomy = Options { picture_memory: 256 << 20, ..Options::default() };
    let packed = run(&Document::from_bytes(bytes).unwrap(), "", &roomy);
    let w = widths(packed.data.expect("smaller"));
    assert!(w.iter().all(|&x| x < 300), "{w:?}");
}

/// Eight pages of the same things, as `repeated`, and the XMP `xmp` in the catalog's metadata.
fn with_xmp(xmp: &str) -> Vec<u8> {
    let pages = 8u32;
    let mut b = PdfBuilder::new();
    let meta = 3 + 6 * pages;
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Metadata {meta} 0 R >>"));
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + 6 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    let grey: Vec<u8> = (0..1600).map(|i| ((i % 40) * 6) as u8).collect();
    for i in 0..pages {
        let n = 3 + 6 * i;
        b.obj(n, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R /Resources << /Font << /F1 {} 0 R >> /XObject << /Im1 {} 0 R >> >> >>", n + 1, n + 2, n + 4));
        b.stream_obj(n + 1, "", format!("BT /F1 12 Tf 20 150 Td (Page {}) Tj ET q 80 0 0 80 20 20 cm /Im1 Do Q", i + 1).as_bytes());
        b.obj(n + 2, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
        b.obj(n + 3, "<< /Type /ExtGState /CA 0.9 >>");
        b.stream_obj(n + 4, "/Type /XObject /Subtype /Image /Width 40 /Height 40 /ColorSpace /DeviceGray /BitsPerComponent 8", &grey);
        b.obj(n + 5, "[1 2 3]");
    }
    b.stream_obj(meta, "/Type /Metadata /Subtype /XML", xmp.as_bytes());
    b.finish_classic(meta + 1, "/Root 1 0 R")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_file_that_says_it_is_pdfa_1_is_written_with_a_classic_table_and_one_that_says_part_2_is_packed() {
    let all = Options::default();
    let part = |n: u32| {
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"><pdfaid:part>{n}</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance></rdf:Description></rdf:RDF></x:xmpmeta>"
        )
    };
    // Part 1: no object streams, no cross-reference stream, a classic table; it is still smaller, and the file says why.
    let input = with_xmp(&part(1));
    let packed = run(&Document::from_bytes(input.clone()).unwrap(), "", &all);
    let out = packed.data.expect("smaller");
    assert!(!contains(&out, b"/ObjStm") && !contains(&out, b"/XRef") && contains(&out, b"\nxref\n") && contains(&out, b"\ntrailer"));
    assert!(packed.warnings.iter().any(|w| w.0.contains("PDF/A-1")), "{:?}", packed.warnings);
    assert!(packed.merged > 0 && out.len() < input.len());
    Document::from_bytes(out).unwrap();
    // Part 2 (or a file that says nothing): packed.
    for xmp in [part(2), String::new()] {
        let packed = run(&Document::from_bytes(with_xmp(&xmp)).unwrap(), "", &all);
        let out = packed.data.expect("smaller");
        assert!(contains(&out, b"/ObjStm") && contains(&out, b"/XRef"), "{xmp}");
        assert!(packed.warnings.iter().all(|w| !w.0.contains("PDF/A-1")));
    }
}
