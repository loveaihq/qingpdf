//! Tests of the JBIG2 decoder: streams made by [`super::test_enc`] must decode to the picture that was encoded, the
//! example of ISO 32000-1 7.4.7 must decode, and hostile streams (built here) must end quickly and cleanly.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::useless_vec, clippy::manual_is_multiple_of)]

use std::time::{Duration, Instant};

use super::bitmap::{Bitmap, Op};
use super::test_enc::*;
use super::{decode, Page};
use crate::error::Error;
use crate::render::work::Work;

/// Decode a stream and give the page back as a bitmap (1 is black).
pub(super) fn page_of(page: &Page, w: usize, h: usize) -> Bitmap {
    let stride = w.div_ceil(8);
    let mut b = Bitmap::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let bit = (page.data[y * stride + x / 8] >> (7 - x % 8)) & 1;
            b.set(x, y, 1 - bit);
        }
    }
    b
}

pub(super) fn run(data: &[u8], globals: Option<&[u8]>, w: usize, h: usize) -> (Bitmap, Option<String>) {
    let work = Work::new();
    let page = decode(data, globals, (w, h), &work).expect("decodes");
    (page_of(&page, w, h), page.warning)
}

/// When the variable is set, the streams the tests make are written there (for the PDFium check).
pub(super) fn dump(name: &str, data: &[u8], globals: Option<&[u8]>, w: usize, h: usize) {
    if let Ok(dir) = std::env::var("JBIG2_DUMP_DIR") {
        std::fs::write(format!("{dir}/{name}.jb2"), data).expect("writes");
        if let Some(g) = globals {
            std::fs::write(format!("{dir}/{name}.globals"), g).expect("writes");
        }
        std::fs::write(format!("{dir}/{name}.size"), format!("{w} {h}")).expect("writes");
    }
}

pub(super) fn show(b: &Bitmap) -> String {
    (0..b.h).map(|y| (0..b.w).map(|x| if b.get(x as i64, y as i64) == 1 { '#' } else { '.' }).collect::<String>()).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_encoder_reproduces_the_standard_test_sequence() {
    // T.88 H.2: the 256 decisions and what the standard says they code to.
    let decoded: [u8; 32] = [
        0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED,
        0x90, 0x4F, 0x46, 0xA3, 0xBF,
    ];
    let coded: [u8; 30] = [
        0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB, 0x86, 0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF,
        0xFF, 0xAC,
    ];
    let mut enc = MqEnc::new();
    let mut cx = 0u8;
    for i in 0..256 {
        enc.encode(u32::from((decoded[i / 8] >> (7 - i % 8)) & 1), &mut cx);
    }
    assert_eq!(enc.finish(), coded);
}

#[test]
fn generic_regions_of_every_template_round_trip() {
    let custom: [[(i8, i8); 4]; 4] = [[(-2, 0), (5, -1), (-8, -3), (3, -2)], [(-6, -2), (0, 0), (0, 0), (0, 0)], [(4, -3), (0, 0), (0, 0), (0, 0)], [(-128, -128), (0, 0), (0, 0), (0, 0)]];
    for template in 0..4u8 {
        for tpgdon in [false, true] {
            for (i, at) in [nominal_at(template), custom[usize::from(template)]].into_iter().enumerate() {
                for (w, h, seed) in [(37usize, 29usize, 1u32), (200, 70, 2), (8, 8, 3), (1, 5, 4)] {
                    let pic = picture(w, h, seed);
                    let data = stream(w as u32, h as u32, vec![segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, template, tpgdon, at))]);
                    let (got, warning) = run(&data, None, w, h);
                    assert!(warning.is_none(), "{warning:?}");
                    assert!(same(&pic, &got), "template {template} tpgdon {tpgdon} at {i} size {w}x{h}\n{}\n\n{}", show(&pic), show(&got));
                    if w == 200 {
                        dump(&format!("generic_t{template}_tp{}_at{i}", u8::from(tpgdon)), &data, None, w, h);
                    }
                }
            }
        }
    }
}

#[test]
fn typical_prediction_with_repeated_rows() {
    // Rows that repeat (and the first row white) are not coded with TPGDON; a picture that is mostly repeats.
    let mut pic = Bitmap::new(90, 60, false);
    for y in 10..40 {
        for x in 20..70 {
            pic.set(x, y, 1);
        }
    }
    for template in 0..4u8 {
        let data = stream(90, 60, vec![segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, template, true, nominal_at(template)))]);
        let (got, _) = run(&data, None, 90, 60);
        assert!(same(&pic, &got), "template {template}");
    }
}

#[test]
fn regions_are_put_on_the_page_with_their_operators_and_clipped() {
    let a = picture(40, 30, 7);
    let b = picture(40, 30, 8);
    let seg_a = segment(1, 38, &[], &generic_region(&a, 10, 5, Op::Or, 0, false, nominal_at(0)));
    for (op, f) in [(Op::Or, (|p: u8, q: u8| p | q) as fn(u8, u8) -> u8), (Op::And, |p, q| p & q), (Op::Xor, |p, q| p ^ q), (Op::Xnor, |p, q| 1 - (p ^ q)), (Op::Replace, |_, q| q)] {
        // The second region hangs over the left and the bottom of the page.
        let seg_b = segment(2, 38, &[], &generic_region(&b, 25, 20, op, 0, false, nominal_at(0)));
        let data = stream(60, 40, vec![seg_a.clone(), seg_b]);
        let (got, _) = run(&data, None, 60, 40);
        let mut want = Bitmap::new(60, 40, false);
        want.combine(&a, 10, 5, Op::Or);
        let mut under = Bitmap::new(60, 40, false);
        under.combine(&b, 25, 20, Op::Replace);
        let mut expect = Bitmap::new(60, 40, false);
        for y in 0..40 {
            for x in 0..60 {
                let (p, q) = (want.get(x, y), under.get(x, y));
                let inside = (25..65).contains(&x) && (20..50).contains(&y);
                expect.set(x as usize, y as usize, if inside { f(p, q) } else { p });
            }
        }
        assert!(same(&expect, &got), "{op:?}");
    }
}

#[test]
fn a_page_that_says_nothing_of_its_size_takes_the_images_and_a_striped_page_grows() {
    let pic = picture(50, 20, 3);
    // No page information at all.
    let data = segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, 0, false, nominal_at(0)));
    let (got, _) = run(&data, None, 50, 20);
    assert!(same(&pic, &got));
    // Page height unknown, two stripes, each ended.
    let (s1, s2) = (picture(50, 12, 4), picture(50, 12, 5));
    let mut v = segment(0, 48, &[], &page_info(50, u32::MAX));
    v.extend(segment(1, 38, &[], &generic_region(&s1, 0, 0, Op::Or, 0, false, nominal_at(0))));
    v.extend(segment(2, 50, &[], &11u32.to_be_bytes()));
    v.extend(segment(3, 38, &[], &generic_region(&s2, 0, 12, Op::Or, 0, false, nominal_at(0))));
    v.extend(segment(4, 50, &[], &23u32.to_be_bytes()));
    let (got, _) = run(&v, None, 50, 24);
    let mut want = Bitmap::new(50, 24, false);
    want.combine(&s1, 0, 0, Op::Or);
    want.combine(&s2, 0, 12, Op::Or);
    assert!(same(&want, &got));
}

#[test]
fn the_example_of_the_pdf_specification_decodes() {
    // ISO 32000-1 7.4.7, example 2: a global symbol dictionary and a page with a text region of two instances.
    let globals = hex("0000000000010000000032000003fffdff02fefefe00000001000000012ae225aea9a5a538b4d9999c5c8e56ef0f8727f2b53d4e37ef795cc5506dffac");
    let stream = hex("00000001300001000000130000003400000042000000000000000040000000000002062000010000001e0000003400000042000000000000000002001000000002") ;
    let mut stream = stream;
    stream.extend(hex("31db51ce51ffac"));
    let (got, warning) = run(&stream, Some(&globals), 52, 66);
    assert!(warning.is_none(), "{warning:?}");
    let black = (0..66).flat_map(|y| (0..52).map(move |x| (x, y))).filter(|&(x, y)| got.get(x, y) == 1).count();
    assert!(black > 100, "{black}\n{}", show(&got));
    dump("spec_example", &stream, Some(&globals), 52, 66);
}

pub(super) fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex")).collect()
}

#[test]
fn hostile_a_generic_region_of_two_to_the_31_rows() {
    // A region that says 2^31 rows (and 1 column) in 20 bytes: it is refused, quickly.
    let mut d = region_info(1, 1 << 31, 0, 0, Op::Or);
    d.push(0);
    d.extend([3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE]);
    d.extend([0x12, 0x34, 0xFF, 0xAC]);
    let data = stream(1, 1, vec![segment(1, 38, &[], &d)]);
    let start = Instant::now();
    let work = Work::new();
    let r = decode(&data, None, (1, 1), &work);
    assert!(start.elapsed() < Duration::from_secs(2));
    // The region is too large: the page (1 by 1) has nothing drawn on it, with a warning.
    let page = r.expect("a page");
    assert!(page.warning.is_some_and(|w| w.contains("too large")), "a warning");
}

#[test]
fn hostile_an_arithmetic_stream_of_ff_for_ever() {
    // A generic region of 4000 x 4000 pixels whose data is 0xFF for ever (no marker is a valid end for the decoder: it
    // is all marker): it must stop when the data is used up, not decode 16 million pixels of nothing.
    let mut d = region_info(4000, 4000, 0, 0, Op::Or);
    d.push(0);
    d.extend([3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE]);
    d.extend(vec![0xFFu8; 64]);
    let data = stream(4000, 4000, vec![segment(1, 38, &[], &d)]);
    let start = Instant::now();
    let work = Work::new();
    let page = decode(&data, None, (4000, 4000), &work).expect("a page");
    assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    assert!(page.warning.is_some_and(|w| w.contains("cut short")));
    assert!(work.used() < 3.0e8, "{}", work.used());
}

#[test]
fn the_meter_ends_a_decode_and_is_charged_for_work_that_fails() {
    let pic = picture(400, 300, 9);
    let data = stream(400, 300, vec![segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, 0, false, nominal_at(0)))]);
    // Enough for the header and clearing but not for the rows.
    let work = Work::with_allowance(100_000.0);
    match decode(&data, None, (400, 300), &work) {
        Err(Error::Limit(_)) => {}
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("decoded with no work left"),
    }
    assert!(work.is_over());
    // A full decode charges for about what the pixels cost.
    let work = Work::new();
    decode(&data, None, (400, 300), &work).expect("decodes");
    assert!(work.used() > 400.0 * 300.0 * 5.0, "{}", work.used());
}

#[test]
fn exported_symbols_are_shared_not_copied() {
    use std::rc::Rc;

    use super::symbol::decode_dictionary;
    use super::test_enc_text::symbol_dict;
    use super::Ctx;
    let syms: Vec<Bitmap> = (0..40).map(|i| picture(6 + i % 5, 9, 90 + i as u32)).collect::<Vec<_>>();
    let mut sorted = syms;
    sorted.sort_by_key(|b| (b.h, b.w));
    let work = Work::new();
    let ctx = Ctx::new(&work);
    let first = decode_dictionary(&ctx, &symbol_dict(&sorted, 0, nominal_at(0), 0, &vec![true; 40]), Vec::new(), &[], None).expect("decodes");
    assert_eq!(first.exported.len(), 40);
    let spent = ctx.memory_left();
    // A dictionary that makes nothing and passes everything on, given the same symbols four times over.
    let inputs: Vec<Rc<Bitmap>> = (0..4).flat_map(|_| first.exported.iter().cloned()).collect();
    let second = decode_dictionary(&ctx, &symbol_dict(&[], 0, nominal_at(0), 160, &vec![true; 160]), inputs, &[], None).expect("decodes");
    assert_eq!(second.exported.len(), 160);
    for (i, s) in second.exported.iter().enumerate() {
        assert!(Rc::ptr_eq(s, &first.exported[i % 40]), "symbol {i} is a copy");
    }
    // No memory was taken for any bitmap beyond the contexts and the like (a copy of 160 symbols would be 160 * 64 bytes).
    assert!(spent - ctx.memory_left() < 160 * 64, "{}", spent - ctx.memory_left());
}

#[test]
fn pages_of_long_runs_round_trip_whatever_the_template() {
    // Mostly blank, with a few marks and a solid block: the stretches where the context does not change are decoded
    // as runs, and what they give must be what the pixel by pixel decoder gives.
    let mut pic = Bitmap::new(333, 90, false);
    for y in 10..30 {
        for x in 40..170 {
            pic.set(x, y, 1);
        }
    }
    for (x, y) in [(5usize, 5usize), (200, 12), (201, 12), (330, 80), (0, 89), (100, 60), (101, 61), (250, 40)] {
        pic.set(x, y, 1);
    }
    for y in 50..90 {
        pic.set(300, y, 1);
    }
    for template in 0..4u8 {
        for tpgdon in [false, true] {
            let data = stream(333, 90, vec![segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, template, tpgdon, nominal_at(template)))]);
            let (got, warning) = run(&data, None, 333, 90);
            assert!(warning.is_none(), "{warning:?}");
            assert!(same(&pic, &got), "template {template} tpgdon {tpgdon}\n{}", show(&got));
        }
    }
    // The same page all black, and all white, in a width that is not a multiple of eight.
    for black in [false, true] {
        let b = Bitmap::new(203, 31, black);
        let data = stream(203, 31, vec![segment(1, 38, &[], &generic_region(&b, 0, 0, Op::Or, 0, false, nominal_at(0)))]);
        let (got, _) = run(&data, None, 203, 31);
        assert!(same(&b, &got), "black {black}");
    }
}

/// A page 100 by 100 with the image `data` (with `globals`) drawn over its lower left 64 by 64 points, at 72 dpi.
fn page_with_jbig2(dict_extra: &str, data: &[u8], globals: Option<&[u8]>, w: usize, h: usize, content: &str) -> crate::document::Document {
    use crate::render::tests::page_doc;
    let dict = format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode {dict_extra}");
    match globals {
        Some(g) => page_doc("", "<< /XObject << /Im 5 0 R >> >>", content.as_bytes(), &[], &[(5, &format!("{dict} /DecodeParms << /JBIG2Globals 6 0 R >>"), data), (6, "", g)]),
        None => page_doc("", "<< /XObject << /Im 5 0 R >> >>", content.as_bytes(), &[], &[(5, &dict, data)]),
    }
}

fn dark(b: &crate::render::Bitmap, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    use crate::render::tests::pixel;
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| pixel(b, x, y)[0] < 128).count()
}

#[test]
fn a_jbig2_image_is_drawn_with_its_globals() {
    use crate::render::tests::{draw, pixel};
    // The example of the specification: 52 by 66 pixels, two letters; drawn at 1:1 with its corner at (10, 24).
    let globals = hex("0000000000010000000032000003fffdff02fefefe00000001000000012ae225aea9a5a538b4d9999c5c8e56ef0f8727f2b53d4e37ef795cc5506dffac");
    let mut data = hex("00000001300001000000130000003400000042000000000000000040000000000002062000010000001e0000003400000042000000000000000002001000000002");
    data.extend(hex("31db51ce51ffac"));
    let doc = page_with_jbig2("", &data, Some(&globals), 52, 66, "q 52 0 0 66 10 24 cm /Im Do Q");
    let (b, warnings) = draw(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");
    let b = b.expect("renders");
    // 234 black pixels, all of them inside the image; nothing outside; the corner of the image is white.
    assert_eq!(dark(&b, 10, 10, 62, 76), 234);
    assert_eq!(dark(&b, 0, 0, 100, 100), 234);
    assert_eq!(pixel(&b, 11, 12), [255, 255, 255]);
}

#[test]
fn a_jbig2_stencil_mask_paints_where_the_page_is_black() {
    use crate::render::tests::{draw, pixel};
    // 16 by 8: the left half black. As an image mask a 0 sample paints, so the left half takes the fill colour.
    let mut pic = Bitmap::new(16, 8, false);
    for y in 0..8 {
        for x in 0..8 {
            pic.set(x, y, 1);
        }
    }
    let data = stream(16, 8, vec![segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, 0, false, nominal_at(0)))]);
    let dict = "/Type /XObject /Subtype /Image /Width 16 /Height 8 /ImageMask true /BitsPerComponent 1 /Filter /JBIG2Decode";
    let doc = crate::render::tests::page_doc("", "<< /XObject << /Im 5 0 R >> >>", b"1 0 0 rg q 80 0 0 40 10 10 cm /Im Do Q", &[], &[(5, dict, &data)]);
    let (b, warnings) = draw(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");
    let b = b.expect("renders");
    assert_eq!(pixel(&b, 20, 70), [255, 0, 0]);
    assert_eq!(pixel(&b, 80, 70), [255, 255, 255]);
}

#[test]
fn a_damaged_jbig2_image_keeps_what_it_has_and_a_hopeless_one_is_a_grey_block() {
    use crate::render::tests::{draw, pixel};
    let pic = picture(64, 64, 4);
    let data = stream(64, 64, vec![segment(1, 38, &[], &generic_region(&pic, 0, 0, Op::Or, 0, false, nominal_at(0)))]);
    // Cut off: the rows that were decoded stay, with a warning. The segment says it is longer than the data, so the
    // page has nothing from it, and the warning says so; the image is the white page.
    let cut = &data[..data.len() - 40];
    let (b, warnings) = draw(&page_with_jbig2("", cut, None, 64, 64, "q 64 0 0 64 0 36 cm /Im Do Q"));
    assert!(warnings.iter().any(|w| w.contains("JBIG2")), "{warnings:?}");
    assert!(b.is_ok());
    // A stream of nothing a JBIG2 decoder knows: a grey block and a warning.
    let (b, warnings) = draw(&page_with_jbig2("", &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20], None, 64, 64, "q 64 0 0 64 0 36 cm /Im Do Q"));
    assert!(warnings.iter().any(|w| w.contains("JBIG2") && w.contains("grey block")), "{warnings:?}");
    let b = b.expect("renders");
    assert_eq!(pixel(&b, 30, 40), [191, 191, 191]);
}
