//! What the review of 3c2-1 found: work and memory that cost nothing on the meter. Each test builds the stream the
//! reviewer's reproducer had (width-0 rows, retained contexts, custom tables), and checks that it ends fast and that
//! the memory the decoder counts is what it holds.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::useless_vec)]

use std::cell::Cell;
use std::time::{Duration, Instant};

use super::bitmap::{Bitmap, Op};
use super::huffman::Table;
use super::test_enc::*;
use super::test_enc_text::*;
use super::{Ctx, GlobalsCache, GlobalsSource, MAX_MEMORY, Page, decode, decode_with_globals, run};
use crate::error::{Error, Result};
use crate::object::ObjRef;
use crate::render::work::Work;

const SECS: u64 = 20;
const AT0: [u8; 8] = [3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE];

/// Decode with a context of our own, to see how much memory it counted when it ended.
fn counted(data: &[u8], w: usize, h: usize) -> (Result<Page>, u64, f64, Duration) {
    let work = Work::new();
    let ctx = Ctx::new(&work);
    let start = Instant::now();
    let r = run(&ctx, data, None, (w, h));
    (r, ctx.memory_left(), work.used(), start.elapsed())
}

fn warns(r: &Result<Page>, text: &str) -> bool {
    matches!(r, Ok(p) if p.warning.as_ref().is_some_and(|w| w.contains(text)))
}

/// A symbol dictionary segment body with these flags, no symbols to decode and `data` after the counts.
fn dict_body(flags: u16, num_new: u32, data: &[u8]) -> Vec<u8> {
    let mut d = flags.to_be_bytes().to_vec();
    d.extend(AT0);
    d.extend(0u32.to_be_bytes());
    d.extend(num_new.to_be_bytes());
    d.extend_from_slice(data);
    d
}

fn halftone_pattern_dict() -> Vec<u8> {
    // Two patterns of 4 by 4, side by side.
    let mut d = vec![0u8, 4, 4];
    d.extend(1u32.to_be_bytes());
    let mut mq = MqEnc::new();
    encode_generic(&mut mq, &mut vec![0u8; 1 << 16], &picture(8, 4, 1), 0, false, [(-4, 0), (-3, -1), (2, -2), (-2, -2)], None);
    d.extend(mq.finish());
    d
}

/// Three hundred regions of no width and 2^20 rows, of each kind that can be kept without being put on the page. The
/// page's work must run out (about 25 million rows are 2 billion units), and quickly.
#[test]
fn rows_of_no_width_are_charged() {
    let tall = region_info(0, 1 << 20, 0, 0, Op::Or);
    let generic = |i: u32| {
        let mut d = tall.clone();
        d.push(0);
        d.extend(AT0);
        d.extend([0u8; 4]);
        segment(i, 36, &[], &d)
    };
    let refinement = |i: u32| {
        // Template 1: no adaptive pixels; refines the part of the page under it.
        let mut d = tall.clone();
        d.push(1);
        d.extend([0u8; 4]);
        segment(i, 40, &[], &d)
    };
    let halftone = |i: u32| {
        // A grid of no columns and 2^20 rows, two patterns (one bit plane).
        let mut d = tall.clone();
        d.push(0);
        d.extend(0u32.to_be_bytes());
        d.extend((1u32 << 20).to_be_bytes());
        d.extend(0i32.to_be_bytes());
        d.extend(0i32.to_be_bytes());
        d.extend(1024u16.to_be_bytes());
        d.extend(0u16.to_be_bytes());
        segment(i, 20, &[1], &d)
    };
    // Symbols of no width and 2^20 rows: one height class, one symbol, exported.
    let symbols = |i: u32| {
        let mut mq = MqEnc::new();
        let (mut iadh, mut iadw, mut iaex) = (vec![0u8; 512], vec![0u8; 512], vec![0u8; 512]);
        enc_int(&mut mq, &mut iadh, Some(1 << 20));
        enc_int(&mut mq, &mut iadw, Some(0));
        enc_int(&mut mq, &mut iadw, None);
        enc_int(&mut mq, &mut iaex, Some(0));
        enc_int(&mut mq, &mut iaex, Some(1));
        segment(i, 0, &[], &dict_body(0, 1, &mq.finish()))
    };
    let kinds: Vec<(&str, Box<dyn Fn(u32) -> Vec<u8>>, usize, Vec<Vec<u8>>)> = vec![
        ("generic", Box::new(generic), 300, Vec::new()),
        ("refinement", Box::new(refinement), 300, Vec::new()),
        ("halftone", Box::new(halftone), 300, vec![segment(1, 16, &[], &halftone_pattern_dict())]),
        ("symbols", Box::new(symbols), 300, Vec::new()),
    ];
    for (name, make, n, first) in kinds {
        let mut segs = first.clone();
        segs.extend((0..n as u32).map(|i| make(i + 2)));
        let (r, _, used, took) = counted(&stream(8, 8, segs), 8, 8);
        assert!(took < Duration::from_secs(SECS), "{name}: {took:?}");
        assert!(matches!(r, Err(Error::Limit(_))), "{name}: {:?}", r.as_ref().map(|p| p.warning.clone()));
        assert!(used <= crate::render::work::PAGE_WORK, "{name}: {used}");
    }
}

/// Twelve thousand symbol dictionaries with nothing in them but the flag that keeps their contexts (72 KB each).
#[test]
fn retained_contexts_are_counted() {
    let body = dict_body(0x0200, 0, &[]);
    let segs = (0..12_000u32).map(|i| segment(i + 1, 0, &[], &body)).collect();
    let (r, left, _, took) = counted(&stream(8, 8, segs), 8, 8);
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    // The cap stops them (about 2600 fit), the stream ends with a warning, and the memory counted is all there is.
    assert!(warns(&r, "more bitmaps than is allowed"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(left < 80_000, "{left}");
}

/// The same segment number again replaces a dictionary and gives its contexts back.
#[test]
fn a_replaced_dictionary_gives_its_memory_back() {
    let body = dict_body(0x0200, 0, &[]);
    let segs = (0..6_000u32).map(|_| segment(1, 0, &[], &body)).collect();
    let (r, left, _, _) = counted(&stream(8, 8, segs), 8, 8);
    assert!(r.as_ref().is_ok_and(|p| p.warning.is_none()), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(MAX_MEMORY - left < 200_000, "{}", MAX_MEMORY - left);
}

/// Three hundred custom Huffman tables of 65536 lines each (two bits a line, all zero): 1.3 MB each.
#[test]
fn custom_tables_are_charged_and_counted() {
    let mut body = vec![0u8];
    body.extend(0i32.to_be_bytes());
    body.extend(65_536i32.to_be_bytes());
    body.extend(vec![0u8; 16_385]);
    let segs = (0..300u32).map(|i| segment(i + 1, 53, &[], &body)).collect();
    let (r, left, used, took) = counted(&stream(8, 8, segs), 8, 8);
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    assert!(warns(&r, "more bitmaps than is allowed"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(left < Table::bytes_for(65_538), "{left}");
    // Each table that was made cost its lines: well over 3 million units.
    let made = (MAX_MEMORY - left) / Table::bytes_for(65_538);
    assert!(made > 100 && used > made as f64 * 3.0e6, "{made} tables, {used}");
}

/// A chain of dictionaries that each pass on 50000 symbols they were given: each keeps a list of them.
#[test]
fn lists_of_exported_symbols_are_counted() {
    const N: i64 = 50_000;
    let mut mq = MqEnc::new();
    let (mut iadh, mut iadw, mut iaex) = (vec![0u8; 512], vec![0u8; 512], vec![0u8; 512]);
    enc_int(&mut mq, &mut iadh, Some(0));
    for _ in 0..N {
        enc_int(&mut mq, &mut iadw, Some(0));
    }
    enc_int(&mut mq, &mut iadw, None);
    enc_int(&mut mq, &mut iaex, Some(0));
    enc_int(&mut mq, &mut iaex, Some(N));
    let mut segs = vec![segment(1, 0, &[], &dict_body(0, N as u32, &mq.finish()))];
    let mut mq = MqEnc::new();
    let mut iaex = vec![0u8; 512];
    enc_int(&mut mq, &mut iaex, Some(0));
    enc_int(&mut mq, &mut iaex, Some(N));
    let pass = dict_body(0, 0, &mq.finish());
    for i in 2..1_500u32 {
        segs.push(segment(i, 0, &[i - 1], &pass));
    }
    let (r, left, _, took) = counted(&stream(8, 8, segs), 8, 8);
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    // 400 KB a dictionary: about 470 of them fit in the cap.
    assert!(warns(&r, "more bitmaps than is allowed"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(left < 500_000, "{left}");
}

/// A new page information segment gives the old page's bitmap back.
#[test]
fn a_replaced_page_gives_its_memory_back() {
    let segs: Vec<Vec<u8>> = (1..400u32).map(|i| segment(i, 48, &[], &page_info(8000, 8000))).collect();
    let (r, left, _, took) = counted(&stream(8, 8, segs), 8, 8);
    assert!(r.is_ok(), "{:?}", r.as_ref().err());
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    // One page of 8 MB is held, not four hundred of them.
    assert!(MAX_MEMORY - left < 9 * 1024 * 1024, "{}", MAX_MEMORY - left);
}

fn glyph_set() -> Vec<Bitmap> {
    let mut syms: Vec<Bitmap> = (0..40).map(|i| picture(6 + i % 5, 9, 90 + i as u32)).collect();
    syms.sort_by_key(|b| (b.h, b.w));
    syms
}

/// Images that name the same globals stream decode it once: a page whose allowance is two images' worth draws five.
#[test]
fn images_that_share_globals_decode_them_once() {
    let syms = glyph_set();
    let globals = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; 40]));
    let o = TextOpts::plain();
    let image = |k: usize| stream(40, 40, vec![segment(2, 6, &[1], &text_region(40, 40, 0, 0, Op::Or, &o, &syms, &[Inst::at(k, 3, 3)]))]);
    let alone = |k: usize| -> (Vec<u8>, f64) {
        let work = Work::new();
        let page = decode(&image(k), Some(&globals), (40, 40), &work).expect("decodes");
        (page.data, work.used())
    };
    let (_, one) = alone(0);
    let loads = Cell::new(0u32);
    let load = || {
        loads.set(loads.get() + 1);
        Some(globals.clone())
    };
    let cache = GlobalsCache::default();
    let work = Work::with_allowance(one * 2.0);
    for k in 0..5 {
        let source = GlobalsSource { key: Some(ObjRef::new(9, 0)), cache: &cache, load: &load };
        let page = decode_with_globals(&image(k), Some(source), (40, 40), &work).expect("decodes");
        assert!(page.warning.is_none(), "{:?}", page.warning);
        assert_eq!(page.data, alone(k).0, "image {k}");
    }
    assert_eq!(loads.get(), 1);
    assert!(!work.is_over());
    // Without the object (no key) each image decodes the globals itself, and the same allowance is not enough.
    let work = Work::with_allowance(one * 2.0);
    let cut = (0..5)
        .filter(|&k| {
            let source = GlobalsSource { key: None, cache: &cache, load: &load };
            matches!(decode_with_globals(&image(k), Some(source), (40, 40), &work), Err(Error::Limit(_)))
        })
        .count();
    assert!(cut >= 2, "{cut}");
}

/// A JBIG2 image drawn twice at one size is decoded once: the images the interpreter keeps for reuse take it too.
#[test]
fn a_jbig2_image_drawn_twice_is_decoded_once() {
    use crate::render::Renderer;
    use crate::render::tests::page_doc;
    let hex = |s: &str| -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex")).collect() };
    // The example of the specification (52 by 66 pixels, two letters).
    let globals = hex("0000000000010000000032000003fffdff02fefefe00000001000000012ae225aea9a5a538b4d9999c5c8e56ef0f8727f2b53d4e37ef795cc5506dffac");
    let mut data = hex("00000001300001000000130000003400000042000000000000000040000000000002062000010000001e0000003400000042000000000000000002001000000002");
    data.extend(hex("31db51ce51ffac"));
    let alone = Work::new();
    decode(&data, Some(&globals), (52, 66), &alone).expect("decodes");
    let used = |content: &str| {
        let dict = "/Type /XObject /Subtype /Image /Width 52 /Height 66 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode /DecodeParms << /JBIG2Globals 6 0 R >>";
        let doc = page_doc("", "<< /XObject << /Im 5 0 R >> >>", content.as_bytes(), &[], &[(5, dict, &data), (6, "", &globals)]);
        let mut r = Renderer::new(&doc);
        let pages = doc.pages().expect("pages");
        r.render_page(&pages[0], 72.0).expect("renders");
        assert!(r.take_warnings().is_empty());
        r.last_work_for_test()
    };
    let once = used("q 52 0 0 66 10 24 cm /Im Do Q");
    let twice = used("q 52 0 0 66 10 24 cm /Im Do Q q 52 0 0 66 10 24 cm /Im Do Q");
    let other_size = used("q 52 0 0 66 10 24 cm /Im Do Q q 26 0 0 33 10 24 cm /Im Do Q");
    // The second draw at the same size costs a copy of the pixels, not a decode. At another size the image is decoded
    // again, but the globals (most of the work here) are the ones the first draw decoded.
    let one = alone.used();
    assert!(twice - once < one / 20.0, "once {once}, twice {twice}, one decode {one}");
    assert!(other_size - once > one / 20.0 && other_size - once < one / 2.0, "once {once}, other size {other_size}, one decode {one}");
}
