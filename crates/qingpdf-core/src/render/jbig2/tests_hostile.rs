//! Streams built to hurt the decoder, the kinds earlier reviews found in the other decoders: a count in a header
//! that is far more than the data can hold, chains that go deep, tables that do not add up, data that stops or that
//! is 0xFF for ever, sizes that need gigabytes. The rule: no panic, no hang, memory held in check, and the page
//! gets what could be decoded (with a warning) or the work meter says the page has had enough.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::useless_vec, clippy::manual_is_multiple_of)]

use std::time::{Duration, Instant};

use super::bitmap::{Bitmap, Op};
use super::test_enc::*;
use super::test_enc_text::*;
use super::tests_text::{glyphs, instances, noisy, table_segment};
use super::{Page, decode};
use crate::error::{Error, Result};
use crate::render::work::Work;

/// Decode, and say how much work it was and how long it took.
fn go(data: &[u8], globals: Option<&[u8]>, w: usize, h: usize) -> (Result<Page>, f64, Duration) {
    let work = Work::new();
    let start = Instant::now();
    let r = decode(data, globals, (w, h), &work);
    (r, work.used(), start.elapsed())
}

/// Did it say so (in the warning of the page it made, or, when there is no page, in the error)?
fn warns(r: &Result<Page>, text: &str) -> bool {
    match r {
        Ok(p) => p.warning.as_ref().is_some_and(|w| w.contains(text)),
        Err(Error::Invalid(m)) => m.contains(text),
        Err(_) => false,
    }
}

const SECS: u64 = 20;

/// A symbol dictionary segment that says how many symbols it has and has `data` for them.
fn dict_header(num_new: u32, flags: u16, data: &[u8]) -> Vec<u8> {
    let mut d = flags.to_be_bytes().to_vec();
    if flags & 1 == 0 {
        d.extend([3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE]);
    }
    if flags & 2 != 0 && flags & 0x1000 == 0 {
        d.extend([0xFF, 0xFF, 0xFF, 0xFF]);
    }
    d.extend(0u32.to_be_bytes());
    d.extend(num_new.to_be_bytes());
    d.extend_from_slice(data);
    d
}

/// Bytes that are not any particular stream, the same each time.
fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 16) as u8
        })
        .collect()
}

#[test]
fn a_huge_symbol_count_with_tiny_data() {
    for (n, flags) in [(0xFFFF_FFFFu32, 0u16), (524_288, 0), (500_000, 0x2), (500_000, 0x1), (500_000, 0x3), (400_000, 0)] {
        let data = stream(50, 50, vec![segment(1, 0, &[], &dict_header(n, flags, &noise(12, 1))), segment(2, 6, &[1], &{
            let mut d = region_info(50, 50, 0, 0, Op::Or);
            d.extend(0u16.to_be_bytes());
            d.extend(10u32.to_be_bytes());
            d.extend(noise(8, 2));
            d
        })]);
        let (r, used, took) = go(&data, None, 50, 50);
        assert!(took < Duration::from_secs(SECS), "{n} {flags}: {took:?}");
        assert!(r.is_ok(), "{n} {flags}: {r:?}", r = r.as_ref().err());
        assert!(used < 2.0e8, "{n} {flags}: {used}");
        assert!(matches!(&r, Ok(p) if p.warning.is_some()), "{n} {flags}");
    }
}

#[test]
fn a_chain_of_two_thousand_aggregates_each_made_of_the_two_before() {
    let a = glyphs();
    let base: Vec<Bitmap> = vec![a[0].clone(), a[1].clone()];
    let mut all = base.clone();
    let mut new_syms = Vec::new();
    let o = TextOpts { refine: true, ..TextOpts::plain() };
    for k in 0..2000usize {
        let insts = vec![Inst::at(all.len() - 1, 0, 0), Inst::at(all.len() - 2, 1, 1)];
        let mut bm = Bitmap::new(8, 8, false);
        bm.combine(&expected_region(8, 8, &o, &all, &insts), 0, 0, Op::Or);
        all.push(bm.clone());
        new_syms.push(NewSymbol::Aggregate { bitmap: bm, insts });
        let _ = k;
    }
    let dict = symbol_dict_agg(&new_syms, 0, [(-1, -1), (-1, -1)], &base, 0);
    let last = all.len() - 1;
    let o2 = TextOpts::plain();
    let data = stream(
        40,
        40,
        vec![
            segment(1, 0, &[], &symbol_dict(&base, 0, nominal_at(0), 0, &[true, true])),
            segment(2, 0, &[1], &dict),
            segment(3, 6, &[2], &text_region(40, 40, 0, 0, Op::Or, &o2, &all, &[Inst::at(last, 5, 5)])),
        ],
    );
    let (r, used, took) = go(&data, None, 40, 40);
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    let page = r.expect("decodes");
    assert!(page.warning.is_none(), "{:?}", page.warning);
    assert!(used < 5.0e8, "{used}");
    // And the same chain asked for at a depth the dictionary does not have the symbols for.
    let mut bad = new_syms;
    bad.truncate(10);
    let dict = symbol_dict_agg(&bad, 0, [(-1, -1), (-1, -1)], &base, 0);
    let mut cut = dict.clone();
    cut.truncate(dict.len() / 2);
    let data = stream(40, 40, vec![segment(1, 0, &[], &symbol_dict(&base, 0, nominal_at(0), 0, &[true, true])), segment(2, 0, &[1], &cut)]);
    let (r, _, took) = go(&data, None, 40, 40);
    assert!(took < Duration::from_secs(SECS));
    assert!(r.is_ok());
}

#[test]
fn an_aggregate_of_sixty_thousand_instances_and_a_dictionary_of_huge_symbols() {
    // REFAGGNINST is read from the data: random data asks for anything; the decoder must not trust it.
    for seed in 0..40u32 {
        let data = stream(30, 30, vec![segment(1, 0, &[], &dict_header(2000, 0x2, &noise(400, seed))), segment(2, 6, &[1], &dict_header(1, 0, &[]))]);
        let (r, used, took) = go(&data, None, 30, 30);
        assert!(took < Duration::from_secs(SECS), "seed {seed}: {took:?}");
        assert!(r.is_ok() || matches!(r, Err(Error::Limit(_))), "seed {seed}");
        assert!(used < 2.1e9);
    }
    // Symbols that are each 100000 pixels wide: the dictionary may not hold them.
    let mut mq = MqEnc::new();
    let (mut iadh, mut iadw) = (vec![0u8; 512], vec![0u8; 512]);
    enc_int(&mut mq, &mut iadh, Some(5000));
    enc_int(&mut mq, &mut iadw, Some(100_000));
    let data = stream(30, 30, vec![segment(1, 0, &[], &dict_header(3, 0, &mq.finish()))]);
    let (r, used, took) = go(&data, None, 30, 30);
    assert!(warns(&r, "too large") || warns(&r, "too many pixels"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(took < Duration::from_secs(SECS) && used < 1.0e8);
}

#[test]
fn a_chain_of_three_thousand_refinement_regions_and_one_of_big_blank_regions() {
    let base = picture(64, 64, 5);
    let mut cur = base.clone();
    let mut segs = vec![segment(1, 36, &[], &generic_region(&base, 0, 0, Op::Or, 0, false, nominal_at(0)))];
    for k in 0..3000u32 {
        let next = noisy(&cur, k, 2);
        let mut d = region_info(64, 64, 0, 0, Op::Replace);
        d.push(1);
        let mut mq = MqEnc::new();
        let mut cx = vec![0u8; 1 << 13];
        encode_refinement(&mut mq, &mut cx, &next, &cur, 0, 0, 1, false, [(0, 0); 2]);
        d.extend(mq.finish());
        let last = k == 2999;
        segs.push(segment(2 + k, if last { 42 } else { 40 }, &[1 + k], &d));
        cur = next;
    }
    let data = stream(64, 64, segs);
    let (r, used, took) = go(&data, None, 64, 64);
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    assert!(r.is_ok(), "{:?}", r.as_ref().err());
    // The chain asked for work in proportion to its pixels.
    assert!(used > 3000.0 * 64.0 * 64.0 * 10.0, "{used}");
    // Blank regions of 10000 by 10000 pixels (12.5 MB each) kept as intermediate regions: the image may not keep them all.
    let blank = Bitmap::new(10_000, 10_000, false);
    let region = generic_region(&blank, 0, 0, Op::Or, 0, true, nominal_at(0));
    // (Unoptimised code takes seconds for each of them: it only tries a few.)
    let count = if cfg!(debug_assertions) { 3 } else { 25 };
    let segs: Vec<Vec<u8>> = (1..=count).map(|n| segment(n, 36, &[], &region)).collect();
    let data = stream(10, 10, segs);
    let (r, used, took) = go(&data, None, 10, 10);
    assert!(took < Duration::from_secs(SECS * 4), "{took:?}");
    if !cfg!(debug_assertions) {
        assert!(warns(&r, "more bitmaps than is allowed") || matches!(r, Err(Error::Limit(_))), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    }
    assert!(used < 2.0e9 + 1.0e9);
}

#[test]
fn bad_huffman_tables_and_symbol_codes() {
    let syms = glyphs();
    let insts = instances(10, 60, 40, syms.len(), 3, false);
    let dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    let o = TextOpts::plain();
    let mut sel = HuffSel::standard();
    sel.fs = 3;
    let good_fs = std::rc::Rc::new(super::huffman::Table::parse_segment(&table_segment(-1024, 1024, 3, 4, &[(2, 10), (2, 10)], 2, 2, None)).expect("table"));
    sel.custom[0] = Some(good_fs);
    let text = segment(3, 6, &[1, 2], &text_region_huffman(60, 40, 0, 0, Op::Or, &o, &sel, &syms, &insts));
    let bad_tables: Vec<Vec<u8>> = vec![
        // The lowest value is not below the highest.
        table_segment(5, 5, 3, 4, &[], 2, 2, None),
        // Three codes of one bit.
        table_segment(-1024, 1024, 3, 4, &[(1, 10), (1, 10)], 1, 1, None),
        // A prefix 255 bits long (8 bit prefix field).
        table_segment(-1024, 1024, 8, 4, &[(255, 10), (255, 10)], 255, 255, None),
        // Two thousand million values in lines of one value each.
        table_segment(i32::MIN, i32::MAX, 3, 4, &[], 2, 2, None),
        // Nothing at all.
        Vec::new(),
        vec![0x25],
    ];
    for (i, t) in bad_tables.into_iter().enumerate() {
        let data = stream(60, 40, vec![dict.clone(), segment(2, 53, &[], &t), text.clone()]);
        let (r, used, took) = go(&data, None, 60, 40);
        assert!(r.is_ok(), "table {i}: {:?}", r.as_ref().err());
        assert!(matches!(&r, Ok(p) if p.warning.is_some()), "table {i}");
        assert!(took < Duration::from_secs(SECS) && used < 1.0e8, "table {i}: {took:?} {used}");
    }
    // Symbol code lengths that run past the last symbol, and a repeat code with nothing before it.
    let mut d = region_info(60, 40, 0, 0, Op::Or);
    d.extend(1u16.to_be_bytes());
    d.extend(0u16.to_be_bytes());
    d.extend(5u32.to_be_bytes());
    let mut bw = BitWriter::default();
    for i in 0..35 {
        bw.bits(if i < 29 { 5 } else { 6 }, 4);
    }
    // Run code 32 first: repeat the previous length, which does not exist.
    bw.bits(0b11110, 5);
    bw.bits(0, 2);
    bw.bytes.extend(noise(20, 4));
    d.extend(bw.bytes);
    let data = stream(60, 40, vec![dict.clone(), segment(2, 6, &[1], &d)]);
    let (r, _, _) = go(&data, None, 60, 40);
    assert!(matches!(&r, Ok(p) if p.warning.is_some()));
}

#[test]
fn truncated_and_damaged_streams_never_panic() {
    let syms = glyphs();
    let insts = instances(20, 80, 50, syms.len(), 7, false);
    let o = TextOpts { refine: true, ..TextOpts::plain() };
    let mut insts2 = insts.clone();
    insts2[0].refined = Some((noisy(&syms[insts2[0].id], 1, 3), 0, 0));
    let streams = vec![
        stream(80, 50, vec![segment(1, 38, &[], &generic_region(&picture(80, 50, 2), 0, 0, Op::Or, 0, true, nominal_at(0)))]),
        stream(
            80,
            50,
            vec![
                segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()])),
                segment(2, 6, &[1], &text_region(80, 50, 0, 0, Op::Or, &o, &syms, &insts2)),
            ],
        ),
        stream(
            80,
            50,
            vec![
                segment(1, 0, &[], &symbol_dict_huffman(&{
                    let mut s = glyphs();
                    s.sort_by_key(|b| (b.h, b.w));
                    s
                }, &vec![true; syms.len()], None)),
                segment(2, 6, &[1], &text_region_huffman(80, 50, 0, 0, Op::Or, &TextOpts::plain(), &HuffSel::standard(), &{
                    let mut s = glyphs();
                    s.sort_by_key(|b| (b.h, b.w));
                    s
                }, &insts)),
            ],
        ),
    ];
    let total = Instant::now();
    for (si, s) in streams.iter().enumerate() {
        for n in 0..s.len() {
            let (r, _, took) = go(&s[..n], None, 80, 50);
            assert!(took < Duration::from_secs(SECS), "stream {si} cut at {n}");
            let _ = r;
        }
        // One byte changed at a time, to every value of a few.
        for i in (0..s.len()).step_by(3) {
            for v in [0x00u8, 0xFF, 0x80] {
                let mut t = s.clone();
                t[i] = v;
                let (r, used, took) = go(&t, None, 80, 50);
                assert!(took < Duration::from_secs(SECS), "stream {si} byte {i} = {v}: {took:?}");
                assert!(used < 2.1e9);
                let _ = r;
            }
        }
    }
    assert!(total.elapsed() < Duration::from_secs(120), "{:?}", total.elapsed());
}

#[test]
fn arithmetic_data_of_ff_for_ever_in_every_kind_of_segment() {
    let ff = vec![0xFFu8; 64];
    let text = |instances: u32, flags: u16, extra: &[u8]| {
        let mut d = region_info(100, 100, 0, 0, Op::Or);
        d.extend(flags.to_be_bytes());
        d.extend_from_slice(extra);
        d.extend(instances.to_be_bytes());
        d.extend_from_slice(&ff);
        d
    };
    let syms = glyphs();
    let dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    let cases: Vec<Vec<Vec<u8>>> = vec![
        vec![segment(1, 0, &[], &dict_header(1000, 0, &ff))],
        vec![segment(1, 0, &[], &dict_header(1000, 0x2, &ff))],
        vec![dict.clone(), segment(2, 6, &[1], &text(1 << 26, 0, &[]))],
        vec![dict.clone(), segment(2, 6, &[1], &text(100_000, 0x2, &[0xFF, 0xFF, 0xFF, 0xFF]))],
        vec![dict.clone(), segment(2, 6, &[1], &text(100_000, 0x2 | 0x4 << 2, &[0xFF, 0xFF, 0xFF, 0xFF]))],
        vec![segment(1, 38, &[], &{
            let mut d = region_info(3000, 3000, 0, 0, Op::Or);
            d.push(0);
            d.extend([3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE]);
            d.extend_from_slice(&ff);
            d
        })],
        vec![segment(1, 42, &[], &{
            let mut d = region_info(1500, 1500, 0, 0, Op::Or);
            d.push(0);
            d.extend([0xFF, 0xFF, 0xFF, 0xFF]);
            d.extend_from_slice(&ff);
            d
        })],
    ];
    for (i, segs) in cases.into_iter().enumerate() {
        let (r, used, took) = go(&stream(100, 100, segs), None, 100, 100);
        assert!(took < Duration::from_secs(SECS), "case {i}: {took:?}");
        assert!(r.is_ok(), "case {i}: {:?}", r.as_ref().err());
        assert!(used < 5.0e8, "case {i}: {used}");
    }
}

#[test]
fn halftone_grids_that_are_too_large_or_large_and_charged() {
    let patterns = vec![picture(4, 4, 1)];
    let pat = {
        let mut d = vec![0u8, 4, 4];
        d.extend(0u32.to_be_bytes());
        let mut mq = MqEnc::new();
        encode_generic(&mut mq, &mut vec![0u8; 1 << 16], &patterns[0], 0, false, [(-4, 0), (-3, -1), (2, -2), (-2, -2)], None);
        d.extend(mq.finish());
        d
    };
    let region = |gw: u32, gh: u32| {
        let mut d = region_info(64, 48, 0, 0, Op::Or);
        d.push(0);
        d.extend(gw.to_be_bytes());
        d.extend(gh.to_be_bytes());
        d.extend(0i32.to_be_bytes());
        d.extend(0i32.to_be_bytes());
        d.extend(1024u16.to_be_bytes());
        d.extend(0u16.to_be_bytes());
        d
    };
    for (gw, gh) in [(1u32 << 31, 2u32), (65_536, 65_536), (1 << 20, 1 << 20)] {
        let data = stream(64, 48, vec![segment(1, 16, &[], &pat), segment(2, 22, &[1], &region(gw, gh))]);
        let (r, used, took) = go(&data, None, 64, 48);
        assert!(warns(&r, "too large"), "{gw}x{gh}: {:?}", r.as_ref().map(|p| p.warning.clone()));
        assert!(took < Duration::from_secs(SECS) && used < 1.0e7);
    }
    // 1500 by 1500 cells (2 million), all of them the one pattern: it takes a while and the meter knows.
    let data = stream(64, 48, vec![segment(1, 16, &[], &pat), segment(2, 22, &[1], &region(1500, 1500))]);
    let (r, used, took) = go(&data, None, 64, 48);
    assert!(r.is_ok());
    assert!(took < Duration::from_secs(SECS * 3), "{took:?}");
    assert!(used > 1500.0 * 1500.0 * 60.0, "{used}");
    // Patterns by the million.
    let mut d = vec![0u8, 255, 255];
    d.extend(u32::MAX.to_be_bytes());
    d.extend(noise(10, 1));
    let (r, _, took) = go(&stream(64, 48, vec![segment(1, 16, &[], &d)]), None, 64, 48);
    assert!(warns(&r, "impossible size"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(took < Duration::from_secs(SECS));
}

#[test]
fn too_many_segments_and_too_many_references() {
    // 70000 segments of the kind that says nothing.
    let mut data = Vec::new();
    for n in 0..70_000u32 {
        data.extend(segment(n, 62, &[], &[]));
    }
    let (r, used, took) = go(&data, None, 10, 10);
    assert!(took < Duration::from_secs(SECS));
    // No page was ever made: the page is the size of the image, blank; and the stream was cut off with a warning.
    assert!(warns(&r, "too many segments"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    assert!(used < 1.0e8);
    // A segment that refers to 5000 segments by the long form.
    let mut v = 1u32.to_be_bytes().to_vec();
    v.push(6);
    v.extend((0xE000_0000u32 | 5000).to_be_bytes());
    v.extend(vec![0u8; 626]);
    v.extend(vec![0u8; 5000]);
    v.push(1);
    v.extend(0u32.to_be_bytes());
    let (r, _, _) = go(&v, None, 10, 10);
    assert!(warns(&r, "refers to too many"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    // A text region that does not say how long it is, and a generic region whose end marker is not there.
    let mut v = 1u32.to_be_bytes().to_vec();
    v.extend([6, 0, 1]);
    v.extend(u32::MAX.to_be_bytes());
    v.extend(vec![0u8; 60]);
    let (r, _, _) = go(&v, None, 10, 10);
    assert!(warns(&r, "without a length"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    let mut v = 1u32.to_be_bytes().to_vec();
    v.extend([38, 0, 1]);
    v.extend(u32::MAX.to_be_bytes());
    v.extend(region_info(10, 10, 0, 0, Op::Or));
    v.extend(vec![0x11u8; 60]);
    let (r, _, _) = go(&v, None, 10, 10);
    assert!(warns(&r, "end marker"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
    // A segment longer than the data.
    let mut s = segment(1, 38, &[], &[0; 40]);
    s.truncate(20);
    let (r, _, _) = go(&s, None, 10, 10);
    assert!(warns(&r, "longer than the data"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
}

#[test]
fn regions_of_impossible_size_in_every_kind() {
    let kinds: [(u8, usize); 5] = [(38, 1), (6, 2), (22, 3), (42, 4), (36, 5)];
    for (kind, tail) in kinds {
        for (w, h) in [(100_000u32, 100_000u32), (u32::MAX, 1), (1 << 31, 1 << 31), (1 << 21, 8)] {
            let mut d = region_info(1, 1, 0, 0, Op::Or);
            d[..4].copy_from_slice(&w.to_be_bytes());
            d[4..8].copy_from_slice(&h.to_be_bytes());
            d.extend(noise(30 + tail, 3));
            let (r, used, took) = go(&stream(10, 10, vec![segment(1, kind, &[], &d)]), None, 10, 10);
            assert!(r.is_ok() && warns(&r, "too large"), "kind {kind} {w}x{h}: {:?}", r.as_ref().map(|p| p.warning.clone()));
            assert!(took < Duration::from_secs(SECS) && used < 1.0e7);
        }
    }
    // A page that says it is as big as an image may never be.
    let mut p = Vec::new();
    p.extend(100_000u32.to_be_bytes());
    p.extend(100_000u32.to_be_bytes());
    p.extend([0u8; 11]);
    let (r, _, _) = go(&segment(0, 48, &[], &p), None, 10, 10);
    assert!(r.is_err() || warns(&r, "too large"), "{:?}", r.as_ref().map(|p| p.warning.clone()));
}

#[test]
fn mmr_regions_of_huge_size_with_no_data() {
    // 100 rows of a million columns, two bytes of data: it is decoded (it is within the limits), as far as there is
    // data, the work charged for the size.
    let mut d = region_info(1 << 20, 100, 0, 0, Op::Or);
    d.push(1);
    d.extend([0x26, 0xA0]);
    let (r, used, took) = go(&stream(10, 10, vec![segment(1, 38, &[], &d)]), None, 10, 10);
    assert!(r.is_ok());
    assert!(took < Duration::from_secs(SECS), "{took:?}");
    assert!(used > 1.0e8, "{used}");
}
