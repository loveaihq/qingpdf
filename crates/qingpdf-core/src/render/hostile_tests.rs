//! Files built to hurt the renderer, found in the review of step 3a: each used to abort the process, hang,
//! or use gigabytes. The rule for all of them: no panic, no hang, memory held in check, and either a
//! picture (with a warning for what was skipped) or a clear error.

use std::time::{Duration, Instant};

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::tests::{WHITE, draw, page_doc, pixel};
use super::*;
use crate::testutil::PdfBuilder;

/// Draw the first page at 72 dpi and say that it took less than `seconds`.
fn quick(doc: &Document, seconds: u64) -> (Result<Bitmap>, Vec<String>) {
    quick_at(doc, 72.0, seconds)
}

fn quick_at(doc: &Document, dpi: f64, seconds: u64) -> (Result<Bitmap>, Vec<String>) {
    let start = Instant::now();
    let mut r = Renderer::new(doc);
    let pages = doc.pages().expect("pages");
    let bitmap = r.render_page(&pages[0], dpi);
    let taken = start.elapsed();
    assert!(taken < Duration::from_secs(seconds), "took {taken:?}");
    (bitmap, r.take_warnings())
}

fn has(warnings: &[String], text: &str) -> bool {
    warnings.iter().any(|w| w.contains(text))
}

#[test]
fn a_lab_range_that_runs_backwards_does_not_abort() {
    // /Range [100 -100 -100 100]: min above max made `clamp` panic.
    let doc = page_doc(
        "",
        "<< /ColorSpace << /Cs [/Lab << /WhitePoint [0.95 1 1.09] /Range [100 -100 -100 100] >>] >> >>",
        b"/Cs cs 50 0 0 sc 0 0 50 50 re f",
        &[],
        &[],
    );
    let (b, w) = quick(&doc, 5);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    // L* 50 is a middle grey (the range is the default one).
    let [r, g, bl] = pixel(&b, 25, 75);
    assert!((100..140).contains(&r) && r.abs_diff(g) < 8 && r.abs_diff(bl) < 8, "{r} {g} {bl}");
}

/// Stitching functions that name the next level 16 times each, `levels` deep, then an exponential one:
/// objects 10 and up.
fn stitch_tree(levels: u32) -> Vec<(u32, String)> {
    let bounds: Vec<String> = (1..16).map(|i| format!("{:.6}", f64::from(i) / 16.0)).collect();
    let mut objs = Vec::new();
    for level in 0..levels {
        let next = format!("{} 0 R ", 11 + level);
        objs.push((
            10 + level,
            format!(
                "<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{}] >>",
                next.repeat(16),
                bounds.join(" "),
                "0 1 ".repeat(16)
            ),
        ));
    }
    objs.push((10 + levels, "<< /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [1 0 0] /N 1 >>".to_string()));
    objs
}

#[test]
fn stitching_functions_that_share_their_parts_load_once() {
    // 16 to the 6th power function objects if every mention loads its own copy.
    let objs = stitch_tree(6);
    let refs: Vec<(u32, &str)> = objs.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let doc = page_doc("", "<< /ColorSpace << /Cs [/Separation /Spot /DeviceRGB 10 0 R] >> >>", b"/Cs cs 0.5 sc 0 0 50 50 re f", &refs, &[]);
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(w.is_empty(), "{w:?}");

    // Distinct functions at every mention can not be shared: the number of function objects is capped, and
    // the colour space is given up on (a warning), at once.
    let mut objs: Vec<(u32, String)> = Vec::new();
    let leaf = "<< /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [1 0 0] /N 1 >> ";
    let top: String = (0..256).map(|i| format!("{} 0 R ", 100 + i)).collect();
    let bounds: Vec<String> = (1..256).map(|i| format!("{:.6}", f64::from(i) / 256.0)).collect();
    objs.push((10, format!("<< /FunctionType 3 /Domain [0 1] /Functions [{top}] /Bounds [{}] /Encode [{}] >>", bounds.join(" "), "0 1 ".repeat(256))));
    for i in 0..256 {
        let b7: Vec<String> = (1..8).map(|k| format!("{:.6}", f64::from(k) / 8.0)).collect();
        objs.push((100 + i, format!("<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{}] >>", leaf.repeat(8), b7.join(" "), "0 1 ".repeat(8))));
    }
    let refs: Vec<(u32, &str)> = objs.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let doc = page_doc("", "<< /ColorSpace << /Cs [/Separation /Spot /DeviceRGB 10 0 R] >> >>", b"/Cs cs 0.5 sc 0 0 50 50 re f", &refs, &[]);
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(has(&w, "could not be read"), "{w:?}");
}

const TYPE3_FONT: &str = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a 6 0 R /b 7 0 R >> \
                          /Encoding << /Type /Encoding /Differences [97 /a /b] >> /FirstChar 97 /LastChar 98 /Widths [1000 1000] >>";

#[test]
fn type3_glyphs_that_show_thousands_of_glyphs_are_charged_to_the_page() {
    // 5000 glyphs, each of which shows 5000 glyphs of its own (the font is still the current one): 25 million.
    let shown = format!("BT /F 10 Tf 0 50 Td ({}) Tj ET", "a".repeat(5000));
    let proc_a = format!("({}) Tj", "b".repeat(5000));
    let doc = page_doc("", "<< /Font << /F 5 0 R >> >>", shown.as_bytes(), &[(5, TYPE3_FONT)], &[(6, "", proc_a.as_bytes()), (7, "", b"")]);
    let (b, _) = quick(&doc, 20);
    assert!(matches!(b, Err(Error::Limit(_))), "{:?}", b.map(|b| b.width));
}

#[test]
fn only_text_mode_3_hides_a_type3_glyph() {
    // 9.3.6: no other mode has any effect on text in a Type 3 font (7 does not clip it away either).
    let square = "1000 0 0 0 1000 1000 d1 100 100 800 800 re f";
    for (mode, drawn) in [(0, true), (1, true), (3, false), (7, true)] {
        let content = format!("BT /F 40 Tf 10 10 Td {mode} Tr 1 0 0 rg (a) Tj ET");
        let doc = page_doc("", "<< /Font << /F 5 0 R >> >>", content.as_bytes(), &[(5, TYPE3_FONT)], &[(6, "", square.as_bytes()), (7, "", b"")]);
        let (b, w) = draw(&doc);
        let b = b.expect("renders");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(pixel(&b, 30, 70) == [255, 0, 0], drawn, "mode {mode}");
    }
}

#[test]
fn nested_clip_masks_stay_under_their_cap() {
    // 256 nested triangles on a 1000 by 1000 page: a megabyte of mask each, 254 MB in all, before the cap.
    let mut content = "q 0 0 m 100 0 l 0 100 l h W n ".repeat(256);
    content.push_str("0 0 1 rg 0 0 100 100 re f");
    let doc = page_doc("", "<< >>", content.as_bytes(), &[], &[]);
    let (b, w) = quick_at(&doc, 720.0, 10);
    let b = b.expect("renders");
    assert!(has(&w, "clip masks use more memory"), "{w:?}");
    assert_eq!(pixel(&b, 100, 900), [0, 0, 255]);

    // One after the other they are given back: no warning however many there are.
    let mut content = "q 0 0 m 100 0 l 0 100 l h W n 0 0 1 rg 0 0 100 100 re f Q ".repeat(300);
    content.push_str("1 0 0 rg 0 0 10 10 re f");
    let doc = page_doc("", "<< >>", content.as_bytes(), &[], &[]);
    let (b, w) = quick_at(&doc, 720.0, 20);
    b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
}

/// A DeviceN image of 1000 by 1000 samples (two inks), tinted by a calculator function of `pops` useless steps.
fn devn_doc(data: &[u8], pops: usize) -> Document {
    let program = format!("{{ {}}}", "0 pop ".repeat(pops));
    let packed = compress_to_vec_zlib(data, 1);
    let image = "/Type /XObject /Subtype /Image /Width 1000 /Height 1000 /BitsPerComponent 8 /ColorSpace [/DeviceN [/A /B] /DeviceRGB 6 0 R] /Filter /FlateDecode";
    page_doc(
        "",
        "<< /XObject << /Im 5 0 R >> >>",
        b"q 100 0 0 100 0 0 cm /Im Do Q",
        &[],
        &[(5, image, packed.as_slice()), (6, "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]", program.as_bytes())],
    )
}

#[test]
fn spot_colour_images_with_slow_tint_functions_are_cut_short() {
    // Samples that are all different (a million of them, 65 thousand pairs) and a 1500 step function: at 720 dpi
    // every pixel of the picture is its own colour to work out.
    let mut x = 12345u32;
    let data: Vec<u8> = (0..2_000_000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect();
    let doc = devn_doc(&data, 1500);
    let (b, w) = quick_at(&doc, 720.0, 30);
    b.expect("renders");
    assert!(has(&w, "colour conversion work"), "{w:?}");

    // A cheap function is not cut short, and a colour is worked out once however many pixels have it.
    let doc = {
        let (a, b) = ([255u8, 0], [0u8, 255]);
        let mut data = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                data.extend_from_slice(if (x + y) % 2 == 0 { &a } else { &b });
            }
        }
        let image = "/Type /XObject /Subtype /Image /Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace [/DeviceN [/A /B] /DeviceRGB 6 0 R]";
        page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im Do Q",
            &[],
            &[(5, image, data.as_slice()), (6, "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]", b"{ 0 }")],
        )
    };
    let (b, w) = quick(&doc, 5);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 12, 12), [255, 0, 0]);
    assert_eq!(pixel(&b, 37, 12), [0, 255, 0]);
}

#[test]
fn a_dash_pattern_that_would_make_billions_of_dashes_draws_a_solid_line() {
    let doc = page_doc("", "<< >>", b"4 w [0.01 0.01] 0 d 0 50 m 100000000 50 l S", &[], &[]);
    let (b, w) = quick(&doc, 10);
    let b = b.expect("renders");
    assert!(has(&w, "dashes"), "{w:?}");
    assert_eq!(pixel(&b, 50, 50), [0, 0, 0]);
    assert_eq!(pixel(&b, 50, 40), WHITE);
    // A fine pattern on a short line is still dashed.
    let (b, w) = draw(&page_doc("", "<< >>", b"4 w [10 10] 0 d 0 50 m 100 50 l S", &[], &[]));
    assert!(w.is_empty(), "{w:?}");
    let b = b.expect("renders");
    assert_eq!((pixel(&b, 5, 50), pixel(&b, 15, 50)), ([0, 0, 0], WHITE));
}

#[test]
fn a_clip_that_collapsed_clips_everything_away() {
    // 8.5.4: a path of one point has nothing inside it.
    let (b, _) = draw(&page_doc("", "<< >>", b"10 10 m W n 0 g 0 0 100 100 re f", &[], &[]));
    assert_eq!(pixel(&b.expect("renders"), 50, 50), WHITE);
    // ... for as long as the q lasts.
    let (b, _) = draw(&page_doc("", "<< >>", b"q 10 10 m W n Q 0 g 0 0 100 100 re f", &[], &[]));
    assert_eq!(pixel(&b.expect("renders"), 50, 50), [0, 0, 0]);
}

#[test]
fn inline_image_sizes_from_the_file_do_not_overflow() {
    // (w * components * bits overflows a usize; in a debug build that panicked, in a release build it wrapped.)
    let content = b"q 100 0 0 100 0 0 cm BI /W 999999999999999 /H 2 /BPC 999999999999999 /CS /G ID abc EI Q 0 g 0 0 10 10 re f";
    let (b, _) = quick(&page_doc("", "<< >>", content, &[], &[]), 5);
    assert!(matches!(b, Err(Error::Limit(_))), "{:?}", b.map(|b| b.width));
    // Merely odd numbers are a warning, and the page goes on.
    let content = b"q 100 0 0 100 0 0 cm BI /W 4 /H 2 /BPC 999999999999999 /CS /G ID abc EI Q 0 g 0 0 10 10 re f";
    let (b, w) = quick(&page_doc("", "<< >>", content, &[], &[]), 5);
    assert_eq!(pixel(&b.expect("renders"), 5, 95), [0, 0, 0]);
    assert!(has(&w, "image is skipped"), "{w:?}");
}

/// `pages` pages, each with a picture of 6500 by 4000 grey samples (26 MB once decoded) of its own.
fn scan_book(pages: u32) -> Document {
    let packed = compress_to_vec_zlib(&vec![0u8; 6500 * 4000], 1);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + 3 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"));
    for i in 0..pages {
        let n = 10 + 3 * i;
        b.obj(n, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im {} 0 R >> >> /Contents {} 0 R >>", n + 2, n + 1));
        b.stream_obj(n + 1, "", b"q 100 0 0 100 0 0 cm /Im Do Q");
        b.stream_obj(n + 2, "/Type /XObject /Subtype /Image /Width 6500 /Height 4000 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode", &packed);
    }
    Document::from_bytes(b.finish_classic(10 + 3 * pages, "/Root 1 0 R")).expect("the test file opens")
}

#[test]
fn a_long_scan_renders_every_page() {
    // 45 pages of 26 MB: 1.17 GB decoded, more than the 1 GiB one budget for the whole document holds.
    let doc = scan_book(45);
    let mut r = Renderer::new(&doc);
    let pages = doc.pages().expect("pages");
    assert_eq!(pages.len(), 45);
    let start = Instant::now();
    for (i, page) in pages.iter().enumerate() {
        let b = r.render_page(page, 8.0).unwrap_or_else(|e| panic!("page {} failed: {e}", i + 1));
        assert_eq!(pixel(&b, 5, 5), [0, 0, 0]);
    }
    assert!(r.take_warnings().is_empty());
    assert!(start.elapsed() < Duration::from_secs(60), "took {:?}", start.elapsed());
}

#[test]
fn one_page_still_has_a_budget() {
    // A page that decodes more than the budget (45 pictures of 26 MB on one page) is refused, not drawn for ever.
    let packed = compress_to_vec_zlib(&vec![0u8; 6500 * 4000], 1);
    let xobjects: String = (0..45).map(|i| format!("/I{i} {} 0 R ", 10 + i)).collect();
    let content: String = (0..45).map(|i| format!("q 100 0 0 100 0 0 cm /I{i} Do Q ")).collect();
    let images: Vec<(u32, &str, &[u8])> = (0..45)
        .map(|i| (10 + i, "/Type /XObject /Subtype /Image /Width 6500 /Height 4000 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode", packed.as_slice()))
        .collect();
    let doc = page_doc("", &format!("<< /XObject << {xobjects} >> >>"), content.as_bytes(), &[], &images);
    let (b, _) = quick_at(&doc, 8.0, 60);
    assert!(matches!(b, Err(Error::Limit(_))), "{:?}", b.map(|b| b.width));
}
