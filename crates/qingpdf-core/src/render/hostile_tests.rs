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

// --- step 3c: layers, shadings, patterns, optional content -------------------------------------------------

#[test]
fn groups_nested_thirty_deep_stop_at_the_layer_cap() {
    // Each form is a group drawn at half opacity (so a layer) and draws the next: 30 deep. Layers nest at most 12 deep.
    let mut forms: Vec<(u32, String, String)> = Vec::new();
    for n in 10..40u32 {
        let content = if n < 39 { format!("/A gs /F{} Do 1 0 0 rg 0 0 50 50 re f", n + 1) } else { "1 0 0 rg 0 0 50 50 re f".to_string() };
        let dict = format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >> /Resources << /ExtGState << /A 6 0 R >> /XObject << /F{} {} 0 R >> >>",
            n + 1,
            n + 1
        );
        forms.push((n, dict, content));
    }
    let streams: Vec<(u32, &str, &[u8])> = forms.iter().map(|(n, d, c)| (*n, d.as_str(), c.as_bytes())).collect();
    let doc = page_doc("", "<< /ExtGState << /A 6 0 R >> /XObject << /F10 10 0 R >> >>", b"/A gs /F10 Do", &[(6, "<< /ca 0.5 >>")], &streams);
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(has(&w, "nested more than 12 deep"), "{w:?}");
}

#[test]
fn a_soft_mask_with_a_huge_box_and_a_soft_mask_that_contains_itself() {
    // The mask group's box is 2 billion points wide: the layer is only as big as the page can show.
    let huge = "/Type /XObject /Subtype /Form /BBox [-1000000000 -1000000000 1000000000 1000000000] /Matrix [1000 0 0 1000 0 0] /Group << /S /Transparency >>";
    let doc = page_doc(
        "",
        "<< /ExtGState << /GS 6 0 R >> >>",
        b"/GS gs 1 0 0 rg 0 0 100 100 re f",
        &[(6, "<< /SMask << /S /Luminosity /G 7 0 R >> >>")],
        &[(7, huge, b"1 g -1000000000 -1000000000 2000000000 2000000000 re f")],
    );
    let (b, w) = quick_at(&doc, 144.0, 10);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 100, 100), [255, 0, 0]);
    // The group that sets the very mask it is the group of.
    let again = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >> /Resources << /ExtGState << /GS 6 0 R >> >>";
    let doc = page_doc(
        "",
        "<< /ExtGState << /GS 6 0 R >> >>",
        b"/GS gs 1 0 0 rg 0 0 100 100 re f",
        &[(6, "<< /SMask << /S /Luminosity /G 7 0 R >> >>")],
        &[(7, again, b"/GS gs 1 g 0 0 50 100 re f")],
    );
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(has(&w, "contains itself"), "{w:?}");
}

#[test]
fn soft_masks_made_without_end_are_charged_to_the_page() {
    // Sixty thousand `gs` that each ask for a mask of their own (the CTM differs): the layers they take are charged, and past
    // the allowance the masks are left out with a warning. No hang, no memory growing.
    let mut content = String::from("1 0 0 rg ");
    for i in 0..60_000 {
        content.push_str(&format!("q 1 0 0 1 {}.5 0 cm /GS gs 0 0 100 100 re f Q ", i % 2));
    }
    let doc = page_doc(
        "",
        "<< /ExtGState << /GS 6 0 R >> >>",
        content.as_bytes(),
        &[(6, "<< /SMask << /S /Alpha /G 7 0 R >> >>")],
        &[(7, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >>", b"0 g 0 0 100 100 re f")],
    );
    let (b, w) = quick_at(&doc, 72.0, 60);
    // Either every mask was made (the page's allowance held it) or the page said it was too much: never a crash.
    assert!(b.is_ok() || matches!(b, Err(Error::Limit(_))), "{w:?}");
}

/// A stream of zeros, deflated.
fn zeros(n: usize) -> Vec<u8> {
    compress_to_vec_zlib(&vec![0u8; n], 1)
}

#[test]
fn meshes_with_millions_of_triangles_are_skipped() {
    // Type 4: 3 million vertices of 6 bytes (flag, x, y, r, g, b).
    let data = zeros(6 * 3_000_000);
    let dict = "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 100 0 100 0 1 0 1 0 1] /Filter /FlateDecode";
    let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh 0 0 1 rg 0 0 10 10 re f", &[], &[(5, dict, &data)]);
    let (b, w) = quick(&doc, 20);
    let b = b.expect("renders");
    assert!(has(&w, "shading is skipped") && has(&w, "triangles"), "{w:?}");
    assert_eq!(pixel(&b, 5, 95), [0, 0, 255]);
    // Type 5: 3 million vertices in rows of 1000.
    let data = zeros(5 * 3_000_000);
    let dict = "/ShadingType 5 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /VerticesPerRow 1000 /Decode [0 100 0 100 0 1 0 1 0 1] /Filter /FlateDecode";
    let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, dict, &data)]);
    let (b, w) = quick(&doc, 20);
    b.expect("renders");
    assert!(has(&w, "shading is skipped"), "{w:?}");
    // Type 6 and 7: a million patches' worth of data.
    for ty in [6, 7] {
        let data = zeros(23 * 1_000_000);
        let dict = format!("/ShadingType {ty} /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 100 0 100 0 1 0 1 0 1] /Filter /FlateDecode");
        let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, &dict, &data)]);
        let (b, w) = quick(&doc, 20);
        b.expect("renders");
        assert!(has(&w, "shading is skipped"), "type {ty}: {w:?}");
    }
}

#[test]
fn meshes_of_huge_triangles_are_charged_by_the_pixels_they_cover() {
    // 300 thousand triangles (under the count cap), each as large as half the page, at 288 dpi: the work allowance of the
    // page runs out and the rest is not drawn, quickly.
    let tri = [0u8, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255];
    let data: Vec<u8> = tri.iter().copied().cycle().take(18 * 300_000).collect();
    let packed = compress_to_vec_zlib(&data, 1);
    let dict = "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 100 0 100 0 1 0 1 0 1] /Filter /FlateDecode";
    let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, dict, &packed)]);
    let (b, w) = quick_at(&doc, 288.0, 20);
    b.expect("renders");
    assert!(has(&w, "meshes cover more than is allowed"), "{w:?}");
}

#[test]
fn a_function_that_is_too_slow_for_a_shading_is_given_up_on() {
    // A type 1 shading whose function takes a thousand steps, over a page of a million pixels: far more steps than a page may spend.
    let program = format!("{{ {}0 }}", "0 pop ".repeat(1000));
    let sh = "<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [100 0 0 100 0 0] /Function 8 0 R >>";
    let doc = page_doc(
        "",
        "<< /Shading << /Sh 5 0 R >> >>",
        b"/Sh sh",
        &[(5, sh)],
        &[(8, "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]", program.as_bytes())],
    );
    let (b, w) = quick_at(&doc, 288.0, 30);
    b.expect("renders");
    assert!(has(&w, "not drawn"), "{w:?}");
}

#[test]
fn tiles_a_millionth_of_a_point_wide_and_a_billion_wide() {
    let tile = |extra: &str| format!("/PatternType 1 /PaintType 1 /TilingType 1 {extra} /Resources << >>");
    let page = |dict: &str, content: &[u8]| page_doc("", "<< /Pattern << /P 5 0 R >> >>", b"/Pattern cs /P scn 0 0 100 100 re f", &[], &[(5, dict, content)]);
    // The cell is smaller than a pixel: it stands for a pixel of its average colour, drawn once.
    let t = tile("/BBox [0 0 0.000001 0.000001] /XStep 0.000001 /YStep 0.000001");
    let (b, w) = quick(&page(&t, b"1 0 0 rg 0 0 1 1 re f"), 10);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 50, 50), [255, 0, 0]);
    // A box a hundred points wide on a step of a millionth: a hundred million neighbours reach into each cell.
    let t = tile("/BBox [0 0 100 100] /XStep 0.000001 /YStep 0.000001");
    let (b, w) = quick(&page(&t, b"1 0 0 rg 0 0 1 1 re f"), 10);
    let b = b.expect("renders");
    assert!(has(&w, "overlap each other too much"), "{w:?}");
    assert_eq!(pixel(&b, 50, 50), WHITE);
    // A cell a billion points wide is sampled down to the pixels a cell may have.
    let t = tile("/BBox [0 0 1000000000 1000000000] /XStep 1000000000 /YStep 1000000000");
    let (b, w) = quick(&page(&t, b"0 0 1 rg 0 0 1000000000 1000000000 re f"), 20);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(pixel(&b, 50, 50), [0, 0, 255]);
    // A strip a billion wide and one point high: a cell may not be more pixels than the cap however it is cut.
    let t = tile("/BBox [0 0 1000000000 1] /XStep 1000000000 /YStep 1");
    let (b, _) = quick(&page(&t, b"0 0 1 rg 0 0 1000000000 1 re f"), 20);
    b.expect("renders");
    // A step of zero.
    let t = tile("/BBox [0 0 10 10] /XStep 0 /YStep 10");
    let (b, w) = quick(&page(&t, b"1 0 0 rg 0 0 5 5 re f"), 10);
    assert_eq!(pixel(&b.expect("renders"), 2, 97), WHITE);
    assert!(has(&w, "step of zero"), "{w:?}");
}

#[test]
fn pattern_cells_are_charged_to_the_page() {
    // Twenty distinct patterns, each with cells of 4 million pixels (a 5000 point step), filled on one page: the pixels of cells a
    // page may make run out, and the rest are not drawn.
    let patterns: String = (0..20).map(|i| format!("/P{i} {} 0 R ", 10 + i)).collect();
    let content: String = (0..20).map(|i| format!("/Pattern cs /P{i} scn {} 0 5 5 re f ", i * 5)).collect();
    let dict = "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 5000 5000] /XStep 5000 /YStep 5000 /Resources << >>";
    let streams: Vec<(u32, &str, &[u8])> = (0..20).map(|i| (10 + i, dict, b"1 0 0 rg 0 0 5000 5000 re f".as_slice())).collect();
    let doc = page_doc("", &format!("<< /Pattern << {patterns} >> >>"), content.as_bytes(), &[], &streams);
    let (b, w) = quick(&doc, 30);
    b.expect("renders");
    assert!(has(&w, "more pattern cells than is allowed"), "{w:?}");
}

#[test]
fn patterns_that_use_themselves_are_drawn_once() {
    // A tiling pattern whose cell is painted with the same pattern; two that use each other.
    let dict = |res: &str| format!("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << /Pattern << {res} >> >>");
    let page = |streams: &[(u32, &str, &[u8])]| page_doc("", "<< /Pattern << /P 5 0 R >> >>", b"/Pattern cs /P scn 0 0 100 100 re f", &[], streams);
    let own = dict("/P 5 0 R");
    let doc = page(&[(5, &own, b"/Pattern cs /P scn 0 0 5 5 re f 0 0 1 rg 5 5 5 5 re f")]);
    let (b, w) = quick(&doc, 10);
    let b = b.expect("renders");
    assert!(has(&w, "uses itself"), "{w:?}");
    // The rest of the cell is still there.
    assert_eq!(pixel(&b, 7, 92), [0, 0, 255]);
    let (a, c) = (dict("/Q 6 0 R"), dict("/P 5 0 R"));
    let doc = page(&[(5, &a, b"/Pattern cs /Q scn 0 0 5 5 re f"), (6, &c, b"/Pattern cs /P scn 0 0 5 5 re f")]);
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(has(&w, "uses itself"), "{w:?}");
    // A shading pattern whose shading is the pattern itself.
    let doc = page_doc(
        "",
        "<< /Pattern << /P 5 0 R >> >>",
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[(5, "<< /PatternType 2 /Shading 5 0 R >>")],
        &[],
    );
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(!w.is_empty());
}

#[test]
fn optional_content_that_names_itself_does_not_loop() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [10 0 R] /D << /OFF [10 0 R] >> >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Properties << /A 20 0 R /B 21 0 R /C 22 0 R >> >> >>");
    b.stream_obj(4, "", b"/OC /A BDC 1 0 0 rg 0 0 30 30 re f EMC /OC /B BDC 0 1 0 rg 30 0 30 30 re f EMC /OC /C BDC 0 0 1 rg 60 0 30 30 re f EMC");
    b.obj(10, "<< /Type /OCG >>");
    b.obj(20, "<< /Type /OCMD /OCGs [20 0 R 20 0 R] /VE [/Or 20 0 R] >>");
    b.obj(21, "<< /Type /OCMD /VE 23 0 R >>");
    b.obj(22, "<< /Type /OCMD /OCGs 22 0 R >>");
    b.obj(23, "[/And 23 0 R [/Not 23 0 R [/Or 23 0 R 21 0 R]]]");
    let doc = Document::from_bytes(b.finish_classic(24, "/Root 1 0 R")).expect("opens");
    let (bitmap, w) = quick(&doc, 10);
    let bitmap = bitmap.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    // What names nothing real counts for nothing: it is shown.
    assert_eq!(pixel(&bitmap, 15, 85), [255, 0, 0]);
    assert_eq!(pixel(&bitmap, 45, 85), [0, 255, 0]);
    assert_eq!(pixel(&bitmap, 75, 85), [0, 0, 255]);
}

#[test]
fn big_layers_nested_stay_under_the_memory_cap() {
    // A page of 16 million pixels and three groups on it, each as big as the page: one layer is 64 MB, the cap; the others
    // are not made (and say so), the page is drawn.
    let group = "/Type /XObject /Subtype /Form /BBox [0 0 1000 1000] /Group << /S /Transparency >> /Resources << /ExtGState << /A 6 0 R >> /XObject << /G2 11 0 R >> >>";
    let inner = "/Type /XObject /Subtype /Form /BBox [0 0 1000 1000] /Group << /S /Transparency >> /Resources << /ExtGState << /A 6 0 R >> /XObject << /G3 12 0 R >> >>";
    let last = "/Type /XObject /Subtype /Form /BBox [0 0 1000 1000] /Group << /S /Transparency >>";
    let doc = page_doc(
        "/MediaBox [0 0 1000 1000]",
        "<< /ExtGState << /A 6 0 R >> /XObject << /G1 10 0 R >> >>",
        b"/A gs /G1 Do",
        &[(6, "<< /ca 0.5 >>")],
        &[(10, group, b"/A gs /G2 Do"), (11, inner, b"/A gs /G3 Do"), (12, last, b"0 0 1 rg 0 0 1000 1000 re f")],
    );
    let (b, w) = quick_at(&doc, 288.0, 60);
    let b = b.expect("renders");
    assert!(has(&w, "transparency layers use more memory than is allowed"), "{w:?}");
    // The blue shows through as many halves as there were layers: at least one.
    let p = pixel(&b, 2000, 2000);
    assert!(p[0] < 255, "{p:?}");
}

// --- step 3c, after the review: work that was not charged in proportion to what it costs -------------------------------
//
// (The timed ones are skipped in a debug build, which is ten to thirty times slower.)
// Each of these used to take seconds to minutes (the reproducers h1 to h5 of the review). They are checked two ways: the
// time (generous, so that a slow machine does not fail them) and the units of the page's work meter they use (exact
// enough to tell the cache from no cache).

/// Draw the first page at `dpi`, say that it took less than `seconds`, and also give the units of work it used.
fn quick_work(doc: &Document, dpi: f64, seconds: u64) -> (Result<Bitmap>, Vec<String>, f64) {
    let start = Instant::now();
    let mut r = Renderer::new(doc);
    let pages = doc.pages().expect("pages");
    let bitmap = r.render_page(&pages[0], dpi);
    let taken = start.elapsed();
    assert!(taken < Duration::from_secs(seconds), "took {taken:?}");
    let used = r.last_work_for_test();
    (bitmap, r.take_warnings(), used)
}

/// A file whose group 10 is off, with a membership dictionary written in the resources that names an array of `n`
/// references to it (object 11), and `content` on a page of 100 by 100 points.
fn ocmd_array_doc(content: &str, properties: &str, n: usize) -> Document {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [10 0 R] /D << /OFF [10 0 R] >> >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Properties << {properties} >> >> >>"));
    b.stream_obj(4, "", content.as_bytes());
    b.obj(10, "<< /Type /OCG /Name (a) >>");
    b.obj(11, &format!("[{}]", "10 0 R ".repeat(n)));
    Document::from_bytes(b.finish_classic(12, "/Root 1 0 R")).expect("opens")
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn a_membership_dictionary_written_in_place_is_worked_out_once() {
    // R3-1: a direct dictionary naming an array of 200 thousand groups, used by 100 `BDC`: each use re-read the array (23 s).
    let content = "/OC /P1 BDC 1 0 0 rg 0 0 10 10 re f EMC\n".repeat(100);
    let doc = ocmd_array_doc(&content, "/P1 << /Type /OCMD /OCGs 11 0 R >>", 200_000);
    let (b, w, used) = quick_work(&doc, 72.0, 5);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    // The group is off, so the squares are not there.
    assert_eq!(pixel(&b, 5, 95), WHITE);
    // One reading of the array (200 thousand groups: 24 million for the groups, 32 million for the parse), not a hundred.
    assert!(used < 100e6, "{used}");
    // The same written in the content stream.
    let content = "/OC << /Type /OCMD /OCGs 11 0 R >> BDC 1 0 0 rg 0 0 10 10 re f EMC\n".repeat(100);
    let doc = ocmd_array_doc(&content, "", 200_000);
    let (b, w, used) = quick_work(&doc, 72.0, 5);
    assert_eq!(pixel(&b.expect("renders"), 5, 95), WHITE);
    assert!(w.is_empty(), "{w:?}");
    assert!(used < 100e6, "{used}");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn membership_dictionaries_that_all_differ_run_out_of_work() {
    // 400 dictionaries that differ (so none is the answer to another), each over the 200 thousand groups: the work meter
    // stops the page, which keeps what it drew and says so.
    let mut content = String::from("0 0 1 rg 50 50 20 20 re f\n");
    for i in 0..400 {
        content.push_str(&format!("/OC << /Type /OCMD /OCGs 11 0 R /X {i} >> BDC 1 0 0 rg 0 0 10 10 re f EMC\n"));
    }
    content.push_str("0 1 0 rg 80 80 10 10 re f\n");
    let doc = ocmd_array_doc(&content, "", 200_000);
    let (b, w, used) = quick_work(&doc, 72.0, 12);
    let b = b.expect("renders");
    assert!(has(&w, "asks for more work"), "{w:?}");
    assert!(used > 1.9e9, "{used}");
    // What was drawn is kept; what came after the stop is not.
    assert_eq!(pixel(&b, 60, 40), [0, 0, 255]);
    assert_eq!(pixel(&b, 85, 15), WHITE);
}

/// The data of a Coons mesh of `n` patches: the first with its 12 points and 4 colours, the others each sharing an edge with the
/// one before. Every point is at the far corner of the 16-bit grid, which `Decode` puts a hundred thousand points away.
fn far_patches(n: usize) -> Vec<u8> {
    let mut data = vec![0u8];
    data.extend(std::iter::repeat_n(0xFFu8, 12 * 4));
    data.extend([10u8, 20, 30].repeat(4));
    let mut next = vec![1u8];
    next.extend(std::iter::repeat_n(0xFFu8, 8 * 4));
    next.extend([10u8, 20, 30].repeat(2));
    for _ in 1..n {
        data.extend_from_slice(&next);
    }
    data
}

const FAR_MESH: &str = "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 100000 0 100000 0 1 0 1 0 1] /Filter /FlateDecode";

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn a_mesh_far_off_the_page_is_read_once_and_charged() {
    // R3-2: 199 thousand patches that are all off the page, drawn by 40 `sh`: the bits were read 40 times (5 s) and the
    // patches that were off the bitmap cost nothing. A mesh is read once now, whatever it holds is charged, and one that
    // would take too much memory once it is read is skipped.
    let packed = compress_to_vec_zlib(&far_patches(199_000), 1);
    let doc = page_doc("", "<< /Shading << /S 5 0 R >> >>", "/S sh\n".repeat(40).as_bytes(), &[], &[(5, FAR_MESH, &packed)]);
    let (b, w, _) = quick_work(&doc, 72.0, 5);
    b.expect("renders");
    assert!(has(&w, "shading is skipped") && has(&w, "more memory than is allowed"), "{w:?}");
    // Under that size it is drawn (nothing shows, it is all off the page), and read once.
    // (Each `sh` is in another place, so that each is drawn and none is found again.)
    let moved = |n: u32| -> String { (0..n).map(|i| format!("q 1 0 0 1 {} 0 cm /S sh Q\n", f64::from(i) * 0.01)).collect() };
    let packed = compress_to_vec_zlib(&far_patches(150_000), 1);
    let doc = page_doc("", "<< /Shading << /S 5 0 R >> >>", moved(40).as_bytes(), &[], &[(5, FAR_MESH, &packed)]);
    let (b, w, used) = quick_work(&doc, 72.0, 5);
    assert_eq!(pixel(&b.expect("renders"), 50, 50), WHITE);
    assert!(w.is_empty(), "{w:?}");
    // 150 thousand patches read (700 units each) and looked at 40 times (100 each): about 700 million, not 4 billion.
    assert!((100e6..1.2e9).contains(&used), "{used}");
    // And 400 `sh` of it would be too much: the page stops, quickly.
    let doc = page_doc("", "<< /Shading << /S 5 0 R >> >>", moved(400).as_bytes(), &[], &[(5, FAR_MESH, &packed)]);
    let (b, w, used) = quick_work(&doc, 72.0, 12);
    b.expect("renders");
    assert!(has(&w, "asks for more work"), "{w:?} {used}");
}

/// A full-page radial shading (object 5) and `content` over a page of 612 by 792 points.
fn radial_page(content: &str) -> Document {
    page_doc(
        "/MediaBox [0 0 612 792]",
        "<< /Shading << /S 5 0 R >> >>",
        content.as_bytes(),
        &[(5, "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [306 396 0 306 396 600] /Function 6 0 R /Extend [true true] >>"), (6, "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>")],
        &[],
    )
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn full_page_shadings_are_charged_by_their_pixels_and_found_again() {
    // R3-3: 300 full-page radial `sh` took 3.2 s at 72 dpi (13 s at 150) and were allowed 130 s.
    // The same shading in the same place is drawn once and put on the page 300 times.
    let doc = radial_page(&"/S sh\n".repeat(300));
    let (b, w, used) = quick_work(&doc, 72.0, 5);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    assert!(pixel(&b, 306, 396)[0] > 240);
    // One shading (2 + 17 per pixel) and 300 times the page put on the page (2.5): about 400 million, not 3 billion.
    assert!(used < 0.8e9, "{used}");
    // In 300 places that differ (so each is drawn): the page runs out of work and stops, keeping what it has.
    let moved: String = (0..300).map(|i| format!("q 1 0 0 1 {} 0 cm /S sh Q\n", f64::from(i) * 0.01)).collect();
    let doc = radial_page(&format!("{moved}0 1 0 rg 0 0 100 100 re f"));
    let (b, w, used) = quick_work(&doc, 72.0, 10);
    let b = b.expect("renders");
    assert!(has(&w, "asks for more work"), "{w:?}");
    assert!(used > 1.9e9, "{used}");
    assert!(pixel(&b, 306, 396)[0] > 240);
    // The square drawn after the stop is not there (the shading covers it).
    assert_ne!(pixel(&b, 20, 770), [0, 255, 0]);
}

/// A page of 612 by 792 points with two soft masks, each a full-page group of a grey: `/A` and `/B`.
fn smask_page(content: &str) -> Document {
    let group = "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /CS /DeviceGray >>";
    page_doc(
        "/MediaBox [0 0 612 792]",
        "<< /ExtGState << /A 5 0 R /B 6 0 R >> >>",
        content.as_bytes(),
        &[(5, "<< /SMask << /Type /Mask /S /Luminosity /G 7 0 R >> >>"), (6, "<< /SMask << /Type /Mask /S /Luminosity /G 8 0 R >> >>")],
        &[(7, group, b"0.5 g 0 0 612 792 re f"), (8, group, b"0.7 g 0 0 612 792 re f")],
    )
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn soft_masks_are_kept_for_the_next_gs_that_asks_for_the_same() {
    // R3-5: two soft masks that alternate, 200 times: each `gs` made its mask again (825 of them were allowed, 6.5 ms each).
    let doc = smask_page(&"q /A gs 1 0 0 rg 0 0 100 100 re f Q q /B gs 0 0 1 rg 0 0 100 100 re f Q\n".repeat(200));
    let (b, w, used) = quick_work(&doc, 72.0, 5);
    let b = b.expect("renders");
    assert!(w.is_empty(), "{w:?}");
    // Both squares are there, through a mask of 0.7 over a mask of 0.5.
    assert!(pixel(&b, 50, 742)[2] > 200);
    // Two masks made, and 400 times the clip times the mask worked out, once for each (they are kept too).
    assert!(used < 40e6, "{used}");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn soft_masks_that_all_differ_run_out_of_work() {
    // Seven positions of the same mask, in turn (more than are kept): each is made again. The page stops when its work is
    // used up, with what it has drawn.
    let content: String = (0..2000).map(|i| format!("q 1 0 0 1 {} 0 cm /A gs 1 0 0 rg 0 0 100 100 re f Q\n", i % 7)).collect();
    let doc = smask_page(&content);
    let (b, w, used) = quick_work(&doc, 72.0, 12);
    let b = b.expect("renders");
    assert!(has(&w, "asks for more work"), "{w:?}");
    assert!(used > 1.9e9, "{used}");
    assert!(pixel(&b, 50, 742)[0] > 200);
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timed: run it with --release")]
fn clips_under_a_soft_mask_are_charged_for_the_whole_mask() {
    // R3-4: with a soft mask in force every clip made a mask of the whole page and was charged only for the clip's area.
    // A page of 15 million pixels and 20 thousand tiny clips.
    let w_pts = 3900;
    let group = format!("/Type /XObject /Subtype /Form /BBox [0 0 {w_pts} {w_pts}] /Group << /S /Transparency /CS /DeviceGray >>");
    let content = format!("/A gs\n{}", "q 10 10 5 5 re W n 10 10 5 5 re f Q\n".repeat(20_000));
    let doc = page_doc(
        &format!("/MediaBox [0 0 {w_pts} {w_pts}]"),
        "<< /ExtGState << /A 5 0 R >> >>",
        content.as_bytes(),
        &[(5, "<< /SMask << /Type /Mask /S /Luminosity /G 7 0 R >> >>")],
        &[(7, &group, format!("0.5 g 0 0 {w_pts} {w_pts} re f").as_bytes())],
    );
    let (b, w, used) = quick_work(&doc, 72.0, 12);
    b.expect("renders");
    assert!(has(&w, "asks for more work"), "{w:?}");
    assert!(used > 1.9e9, "{used}");
}

#[test]
fn patterns_are_kept_by_bytes_like_forms() {
    // R3-6: every tiling pattern read was kept, with its decoded content (up to 256 MB each). Forty patterns of 4 MB.
    let big = compress_to_vec_zlib(&vec![b' '; 4 * 1024 * 1024], 1);
    let names: String = (0..40).map(|i| format!("/P{i} {} 0 R ", 10 + i)).collect();
    let content: String = (0..40).map(|i| format!("/Pattern cs /P{i} scn {} 0 2 2 re f ", i * 2)).collect();
    let dict = "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 2 2] /XStep 2 /YStep 2 /Resources << >> /Filter /FlateDecode";
    let streams: Vec<(u32, &str, &[u8])> = (0..40).map(|i| (10 + i, dict, big.as_slice())).collect();
    let doc = page_doc("", &format!("<< /Pattern << {names} >> >>"), content.as_bytes(), &[], &streams);
    let mut r = Renderer::new(&doc);
    let pages = doc.pages().expect("pages");
    r.render_page(&pages[0], 72.0).expect("renders");
    let (bytes, patterns, _) = r.object_cache_for_test();
    assert!(bytes <= 64 * 1024 * 1024, "{bytes}");
    assert!(patterns < 40, "{patterns}");
    // The patterns that were not kept are read again, and still draw.
    let again = r.render_page(&pages[0], 72.0).expect("renders");
    assert_eq!(pixel(&again, 1, 99), WHITE);
    let (bytes, _, _) = r.object_cache_for_test();
    assert!(bytes <= 64 * 1024 * 1024, "{bytes}");
}

#[test]
fn forms_inside_pattern_cells_inside_forms_are_nested_a_bounded_depth_in_all() {
    // R3-8: a pattern cell started the count of nested forms again, so twelve patterns with a chain of eleven forms each
    // made a nest 130 deep. The count goes on through the cells now.
    let levels = 12u32;
    let chain = 11u32;
    let mut streams: Vec<(u32, String, String)> = Vec::new();
    // Patterns: 100 + level. Forms: 1000 + 100 * level + k.
    for level in 1..=levels {
        for k in 0..chain {
            let n = 1000 + 100 * level + k;
            let (res, body) = if k + 1 < chain {
                (format!("<< /XObject << /X {} 0 R >> >>", n + 1), "/X Do".to_string())
            } else if level < levels {
                (format!("<< /Pattern << /P {} 0 R >> >>", 100 + level + 1), "/Pattern cs /P scn 0 0 10 10 re f".to_string())
            } else {
                ("<< >>".to_string(), "1 0 0 rg 0 0 10 10 re f".to_string())
            };
            streams.push((n, format!("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources {res}"), body));
        }
        streams.push((
            100 + level,
            format!("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << /XObject << /X {} 0 R >> >>", 1000 + 100 * level),
            "/X Do".to_string(),
        ));
    }
    let list: Vec<(u32, &str, &[u8])> = streams.iter().map(|(n, d, c)| (*n, d.as_str(), c.as_bytes())).collect();
    let doc = page_doc("", "<< /Pattern << /P 101 0 R >> >>", b"/Pattern cs /P scn 0 0 100 100 re f", &[], &list);
    let (b, w) = quick(&doc, 10);
    b.expect("renders");
    assert!(has(&w, "nested more than 24 deep"), "{w:?}");
}
