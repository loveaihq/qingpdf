//! Round trips for dictionaries, text regions (arithmetic and Huffman), refinement, halftones, MMR: what the test
//! encoder codes must come back out of the decoder.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::useless_vec, clippy::manual_is_multiple_of)]

use std::rc::Rc;

use super::bitmap::{Bitmap, Op};
use super::huffman::Table;
use super::test_enc::*;
use super::test_enc_text::*;
use super::tests::{dump, run, show};

/// Symbols sorted by height, then width, as a dictionary lists them.
pub(super) fn glyphs() -> Vec<Bitmap> {
    [(5usize, 8usize), (7, 8), (4, 10), (9, 10), (6, 12), (8, 12)].iter().enumerate().map(|(i, &(w, h))| picture(w, h, 40 + i as u32)).collect()
}

pub(super) fn instances(n: usize, w: i64, h: i64, symbols: usize, seed: u32, outside: bool) -> Vec<Inst> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(99);
    let mut next = move |m: i64| -> i64 {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        i64::from(s >> 8) % m.max(1)
    };
    (0..n)
        .map(|_| {
            let (x, y) = if outside { (next(w + 6) - 3, next(h + 6) - 3) } else { (next(w - 8), next(h - 12)) };
            Inst::at(next(symbols as i64) as usize, x, y)
        })
        .collect()
}

fn dict_and_text(syms: &[Bitmap], o: &TextOpts, insts: &[Inst], page: (usize, usize), region: (usize, usize, i64, i64)) -> Vec<u8> {
    let export = vec![true; syms.len()];
    stream(
        page.0 as u32,
        page.1 as u32,
        vec![
            segment(1, 0, &[], &symbol_dict(syms, 0, nominal_at(0), 0, &export)),
            segment(2, 6, &[1], &text_region(region.0, region.1, region.2, region.3, Op::Or, o, syms, insts)),
        ],
    )
}

fn expect_page(page: (usize, usize), region: (usize, usize, i64, i64), o: &TextOpts, syms: &[Bitmap], insts: &[Inst]) -> Bitmap {
    let mut want = Bitmap::new(page.0, page.1, false);
    want.combine(&expected_region(region.0, region.1, o, syms, insts), region.2, region.3, Op::Or);
    want
}

#[test]
fn text_regions_of_every_corner_strip_and_offset_round_trip() {
    let syms = glyphs();
    let page = (110usize, 90usize);
    let region = (90usize, 70usize, 8i64, 6i64);
    let insts = instances(45, 90, 70, syms.len(), 5, true);
    let mut count = 0;
    for corner in 0..4u8 {
        for transposed in [false, true] {
            for log_strips in 0..4u32 {
                for ds_offset in [0i64, -4, 7] {
                    for (op, default_black) in [(Op::Or, false), (Op::Xor, true)] {
                        let o = TextOpts { corner, transposed, log_strips, ds_offset, op, default_black, ..TextOpts::plain() };
                        let data = dict_and_text(&syms, &o, &insts, page, region);
                        let (got, warning) = run(&data, None, page.0, page.1);
                        assert!(warning.is_none(), "{warning:?}");
                        let want = expect_page(page, region, &o, &syms, &insts);
                        assert!(same(&want, &got), "corner {corner} transposed {transposed} strips {log_strips} offset {ds_offset} {op:?}\n{}", show(&got));
                        if ds_offset == 0 && log_strips != 1 && op == Op::Or {
                            dump(&format!("text_c{corner}_t{}_s{log_strips}", u8::from(transposed)), &data, None, page.0, page.1);
                        } else if ds_offset == -4 && log_strips == 2 && op == Op::Xor {
                            dump(&format!("text_xor_c{corner}_t{}", u8::from(transposed)), &data, None, page.0, page.1);
                        }
                        count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(count, 4 * 2 * 4 * 3 * 2);
}

#[test]
fn a_dictionary_exports_what_it_was_given_and_what_it_made_and_text_regions_use_several() {
    let a = glyphs();
    let b: Vec<Bitmap> = [(6usize, 14usize), (9, 14), (5, 16)].iter().enumerate().map(|(i, &(w, h))| picture(w, h, 70 + i as u32)).collect();
    // B is given A's symbols, makes three, exports A0, A2, A5, B1 and B2.
    let export_b = [true, false, true, false, false, true, false, true, true];
    let seg_a = segment(1, 0, &[], &symbol_dict(&a, 0, nominal_at(0), 0, &vec![true; a.len()]));
    let seg_b = segment(2, 0, &[1], &symbol_dict(&b, 0, nominal_at(0), a.len(), &export_b));
    let mut all_b: Vec<Bitmap> = a.clone();
    all_b.extend(b.iter().cloned());
    let exported_b: Vec<Bitmap> = all_b.iter().zip(export_b).filter(|(_, e)| *e).map(|(s, _)| s.clone()).collect();
    // A text region that refers to both A and B sees A's six symbols and then B's five.
    let mut syms = a.clone();
    syms.extend(exported_b.iter().cloned());
    let insts = instances(60, 100, 60, syms.len(), 9, false);
    let o = TextOpts::plain();
    let seg_t = segment(3, 6, &[1, 2], &text_region(100, 60, 0, 0, Op::Or, &o, &syms, &insts));
    let data = stream(100, 60, vec![seg_a, seg_b, seg_t]);
    let (got, warning) = run(&data, None, 100, 60);
    assert!(warning.is_none(), "{warning:?}");
    assert!(same(&expected_region(100, 60, &o, &syms, &insts), &got));
    dump("dict_chain", &data, None, 100, 60);
    // The dictionaries of the globals: the same stream with A in the globals stream.
    let globals = segment(1, 0, &[], &symbol_dict(&a, 0, nominal_at(0), 0, &vec![true; a.len()]));
    let page_stream = stream(100, 60, vec![segment(2, 0, &[1], &symbol_dict(&b, 0, nominal_at(0), a.len(), &export_b)), segment(3, 6, &[1, 2], &text_region(100, 60, 0, 0, Op::Or, &o, &syms, &insts))]);
    let (got2, _) = run(&page_stream, Some(&globals), 100, 60);
    assert!(same(&got, &got2));
    dump("dict_globals", &page_stream, Some(&globals), 100, 60);
}

/// `b` with some pixels turned over.
pub(super) fn noisy(b: &Bitmap, seed: u32, flips: usize) -> Bitmap {
    let mut out = Bitmap::new(b.w, b.h, false);
    out.combine(b, 0, 0, Op::Replace);
    let mut s = seed;
    for _ in 0..flips {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let x = (s >> 8) as usize % b.w.max(1);
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let y = (s >> 8) as usize % b.h.max(1);
        out.set(x, y, 1 - out.get(x as i64, y as i64));
    }
    out
}

#[test]
fn refined_instances_round_trip() {
    let syms = glyphs();
    for rtemplate in 0..2u8 {
        for rat in [[(-1, -1), (-1, -1)], [(-3, -1), (2, 2)]] {
            let mut insts = instances(30, 90, 60, syms.len(), 11, false);
            for (k, inst) in insts.iter_mut().enumerate() {
                if k % 3 == 0 {
                    let s = &syms[inst.id];
                    // Same size, a size bigger, a size smaller.
                    let (w, h) = match k % 9 {
                        0 => (s.w, s.h),
                        3 => (s.w + 2, s.h + 1),
                        _ => (s.w - 1, s.h - 2),
                    };
                    let mut r = Bitmap::new(w, h, false);
                    r.combine(&noisy(s, k as u32, 6), 0, 0, Op::Replace);
                    inst.refined = Some((r, (k as i64 % 3) - 1, 1 - (k as i64 % 2)));
                }
            }
            let o = TextOpts { refine: true, rtemplate, rat, ..TextOpts::plain() };
            let data = dict_and_text(&syms, &o, &insts, (100, 80), (90, 60, 0, 0));
            let (got, warning) = run(&data, None, 100, 80);
            assert!(warning.is_none(), "{warning:?}");
            assert!(same(&expect_page((100, 80), (90, 60, 0, 0), &o, &syms, &insts), &got), "rtemplate {rtemplate} rat {rat:?}\n{}", show(&got));
            dump(&format!("text_refine_t{rtemplate}_{}", u8::from(rat[0] != (-1, -1))), &data, None, 100, 80);
        }
    }
}

#[test]
fn refined_instances_with_every_kind_of_offset() {
    // One refined instance per stream; the size changes and the offsets run through their ranges (positive and
    // negative, odd and even sizes: the half of RDW is rounded down).
    let syms = glyphs();
    let mut n = 0;
    for rdw in [-2i64, -1, 0, 1, 2, 3] {
        for rdh in [-1i64, 0, 2] {
            for (rdx, rdy) in [(0i64, 0i64), (-3, 1), (1, -2), (2, 2), (3, -1)] {
                let id = (n % 6) as usize;
                let (w, h) = ((syms[id].w as i64 + rdw) as usize, (syms[id].h as i64 + rdh) as usize);
                let mut r = Bitmap::new(w, h, false);
                r.combine(&noisy(&syms[id], n as u32, 4), 0, 0, Op::Replace);
                let insts = vec![Inst { id, x: 8, y: 8, refined: Some((r, rdx, rdy)) }];
                for rtemplate in 0..2u8 {
                    let o = TextOpts { refine: true, rtemplate, ..TextOpts::plain() };
                    let data = dict_and_text(&syms, &o, &insts, (40, 40), (40, 40, 0, 0));
                    let (got, warning) = run(&data, None, 40, 40);
                    assert!(warning.is_none(), "{warning:?}");
                    assert!(same(&expect_page((40, 40), (40, 40, 0, 0), &o, &syms, &insts), &got), "rdw {rdw} rdh {rdh} rdx {rdx} rdy {rdy}");
                    dump(&format!("refine_offsets_{rdw}_{rdh}_{rdx}_{rdy}_t{rtemplate}").replace('-', "m"), &data, None, 40, 40);
                }
                n += 1;
            }
        }
    }
}

#[test]
fn huffman_refined_instances_with_every_kind_of_offset() {
    let syms = glyphs();
    let seg_dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    let mut n = 0;
    for rdw in [-2i64, 0, 2] {
        for rdh in [0i64, 2] {
            for (rdx, rdy) in [(0i64, 0i64), (-2, 1), (1, -2), (2, 2)] {
                for table in [0u16, 1] {
                    let id = (n % 6) as usize;
                    let (w, h) = ((syms[id].w as i64 + rdw) as usize, (syms[id].h as i64 + rdh) as usize);
                    let mut r = Bitmap::new(w, h, false);
                    r.combine(&noisy(&syms[id], n as u32, 4), 0, 0, Op::Replace);
                    let insts = vec![Inst { id, x: 8, y: 8, refined: Some((r, rdx, rdy)) }];
                    let o = TextOpts { refine: true, ..TextOpts::plain() };
                    let sel = HuffSel { rdw: table, rdh: table, rdx: table, rdy: table, ..HuffSel::standard() };
                    let data = stream(40, 40, vec![seg_dict.clone(), segment(2, 6, &[1], &text_region_huffman(40, 40, 0, 0, Op::Or, &o, &sel, &syms, &insts))]);
                    dump(&format!("hrefine_{rdw}_{rdh}_{rdx}_{rdy}_b{}", 14 + table).replace('-', "m"), &data, None, 40, 40);
                    let (got, warning) = run(&data, None, 40, 40);
                    assert!(warning.is_none(), "{warning:?}");
                    assert!(same(&expected_region(40, 40, &o, &syms, &insts), &got), "rdw {rdw} rdh {rdh} rdx {rdx} rdy {rdy}");
                    n += 1;
                }
            }
        }
    }
}

#[test]
fn huffman_refinements_of_several_instances_keep_their_statistics() {
    // Instances refined at their own size (the offsets vary): the statistics of the refinement coder last for the
    // whole region, only the coder is started again for each.
    let syms = glyphs();
    let mut insts = instances(16, 70, 40, syms.len(), 77, false);
    for (k, inst) in insts.iter_mut().enumerate() {
        let s = &syms[inst.id];
        inst.refined = Some((noisy(s, k as u32, 4), (k % 3) as i64 - 1, (k % 2) as i64));
    }
    let seg_dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    let o = TextOpts { refine: true, ..TextOpts::plain() };
    for table in [0u16, 1] {
        let sel = HuffSel { rdw: table, rdh: table, rdx: table, rdy: table, ..HuffSel::standard() };
        let data = stream(70, 40, vec![seg_dict.clone(), segment(2, 6, &[1], &text_region_huffman(70, 40, 0, 0, Op::Or, &o, &sel, &syms, &insts))]);
        dump(&format!("hmulti_b{}", 14 + table), &data, None, 70, 40);
        let (got, warning) = run(&data, None, 70, 40);
        assert!(warning.is_none(), "{warning:?}");
        assert!(same(&expected_region(70, 40, &o, &syms, &insts), &got));
    }
}

#[test]
fn aggregate_symbols_in_a_dictionary_round_trip() {
    let a = glyphs();
    // New symbols: one refined from input 3, one made of three instances (one of them refined) of the inputs,
    // one made of two instances of the symbols made before it.
    let refined = noisy(&a[3], 5, 5);
    let mut part_refined = Bitmap::new(a[1].w + 1, a[1].h, false);
    part_refined.combine(&noisy(&a[1], 6, 4), 0, 0, Op::Replace);
    let agg_insts = vec![Inst::at(0, 0, 0), Inst { id: 1, x: 5, y: 2, refined: Some((part_refined, 0, 0)) }, Inst::at(4, 3, 4)];
    let mut agg = Bitmap::new(16, 16, false);
    {
        let o = TextOpts { refine: true, ..TextOpts::plain() };
        agg.combine(&expected_region(16, 16, &o, &a, &agg_insts), 0, 0, Op::Or);
    }
    let mut all = a.clone();
    let new_syms = vec![
        NewSymbol::Refined { bitmap: refined.clone(), id: 3, rdx: 0, rdy: 0 },
        NewSymbol::Aggregate { bitmap: agg.clone(), insts: agg_insts },
    ];
    // Heights: 10 (refined), 16 (aggregate): ascending. The third needs the first two as inputs.
    all.push(refined.clone());
    all.push(agg.clone());
    let mut third = Bitmap::new(20, 20, false);
    let third_insts = vec![Inst::at(6, 0, 0), Inst::at(7, 6, 7)];
    {
        let o = TextOpts { refine: true, ..TextOpts::plain() };
        third.combine(&expected_region(20, 20, &o, &all, &third_insts), 0, 0, Op::Or);
    }
    let new_syms = {
        let mut v = new_syms;
        v.push(NewSymbol::Aggregate { bitmap: third.clone(), insts: third_insts });
        v
    };
    for rtemplate in 0..2u8 {
        let dict = symbol_dict_agg(&new_syms, rtemplate, [(-1, -1), (-1, -1)], &a, 0);
        let mut syms = a.clone();
        syms.extend(new_syms.iter().map(|n| n.bitmap().clone()));
        let insts: Vec<Inst> = (0..syms.len()).map(|i| Inst::at(i, 3 + (i as i64 % 3) * 30, 2 + (i as i64 / 3) * 22)).collect();
        let o = TextOpts::plain();
        let data = stream(
            100,
            60,
            vec![
                segment(1, 0, &[], &symbol_dict(&a, 0, nominal_at(0), 0, &vec![true; a.len()])),
                segment(2, 0, &[1], &dict),
                segment(3, 6, &[2], &text_region(100, 60, 0, 0, Op::Or, &o, &syms, &insts)),
            ],
        );
        let (got, warning) = run(&data, None, 100, 60);
        assert!(warning.is_none(), "{warning:?}");
        assert!(same(&expected_region(100, 60, &o, &syms, &insts), &got), "rtemplate {rtemplate}
{}", show(&got));
        dump(&format!("aggregate_t{rtemplate}"), &data, None, 100, 60);
    }
}

#[test]
fn huffman_text_regions_with_the_standard_tables_round_trip() {
    let syms = glyphs();
    let page = (110usize, 90usize);
    let region = (90usize, 70usize, 8i64, 6i64);
    // Strips and positions that are not negative: the Huffman tables for the strip steps start at 1.
    let insts = instances(40, 90, 70, syms.len(), 21, false);
    let seg_dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    let mut n = 0;
    for fs in 0..2u16 {
        for ds in 0..3u16 {
            for dt in 0..3u16 {
                for (corner, transposed, log_strips) in [(1u8, false, 0u32), (0, false, 2), (3, true, 1), (2, true, 3)] {
                    let sel = HuffSel { fs, ds, dt, ..HuffSel::standard() };
                    let o = TextOpts { corner, transposed, log_strips, ..TextOpts::plain() };
                    let seg_text = segment(2, 6, &[1], &text_region_huffman(region.0, region.1, region.2, region.3, Op::Or, &o, &sel, &syms, &insts));
                    let data = stream(page.0 as u32, page.1 as u32, vec![seg_dict.clone(), seg_text]);
                    let (got, warning) = run(&data, None, page.0, page.1);
                    assert!(warning.is_none(), "fs {fs} ds {ds} dt {dt} corner {corner}: {warning:?}");
                    assert!(same(&expect_page(page, region, &o, &syms, &insts), &got), "fs {fs} ds {ds} dt {dt} corner {corner} transposed {transposed} strips {log_strips}\n{}", show(&got));
                    if corner == 1 || (fs == 1 && ds == 2 && dt == 2) {
                        dump(&format!("huff_text_fs{fs}_ds{ds}_dt{dt}_c{corner}"), &data, None, page.0, page.1);
                    }
                    n += 1;
                }
            }
        }
    }
    assert_eq!(n, 72);
}

#[test]
fn huffman_text_with_refinement_and_every_refinement_table() {
    let syms = glyphs();
    let mut insts = instances(24, 90, 60, syms.len(), 23, false);
    for (k, inst) in insts.iter_mut().enumerate() {
        if k % 2 == 0 {
            let s = &syms[inst.id];
            let mut r = Bitmap::new(s.w + (k % 3), s.h, false);
            r.combine(&noisy(s, k as u32, 5), 0, 0, Op::Replace);
            inst.refined = Some((r, (k % 4) as i64 - 1, (k % 3) as i64 - 1));
        }
    }
    let seg_dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    for (rdw, rdx) in [(0u16, 0u16), (1, 1), (0, 1)] {
        let sel = HuffSel { rdw, rdh: rdw, rdx, rdy: rdx, ..HuffSel::standard() };
        let o = TextOpts { refine: true, ..TextOpts::plain() };
        let seg_text = segment(2, 6, &[1], &text_region_huffman(90, 60, 0, 0, Op::Or, &o, &sel, &syms, &insts));
        let data = stream(90, 60, vec![seg_dict.clone(), seg_text]);
        dump(&format!("huff_refine_{rdw}{rdx}"), &data, None, 90, 60);
        let (got, warning) = run(&data, None, 90, 60);
        assert!(warning.is_none(), "{warning:?}");
        assert!(same(&expected_region(90, 60, &o, &syms, &insts), &got), "rdw {rdw} rdx {rdx}\n{}", show(&got));
    }
}

#[test]
fn huffman_text_with_one_refined_instance() {
    let syms = glyphs();
    for k in 0..12u32 {
        let id = (k % 6) as usize;
        let mut r = Bitmap::new(syms[id].w + (k as usize % 3), syms[id].h, false);
        r.combine(&noisy(&syms[id], k, 3 + k as usize % 5), 0, 0, Op::Replace);
        let insts = vec![Inst { id, x: 10, y: 10, refined: Some((r, (k % 3) as i64 - 1, (k % 2) as i64)) }];
        let o = TextOpts { refine: true, ..TextOpts::plain() };
        let data = stream(
            50,
            40,
            vec![
                segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()])),
                segment(2, 6, &[1], &text_region_huffman(50, 40, 0, 0, Op::Or, &o, &HuffSel::standard(), &syms, &insts)),
            ],
        );
        dump(&format!("huff_refine_single{k}"), &data, None, 50, 40);
        let (got, warning) = run(&data, None, 50, 40);
        assert!(warning.is_none(), "{warning:?}");
        assert!(same(&expected_region(50, 40, &o, &syms, &insts), &got));
    }
}

/// A table segment: `lines` are (prefix length, range length) from `low` to `high`.
pub(super) fn table_segment(low: i32, high: i32, ps: u32, rs: u32, lines: &[(u8, u8)], lower: u8, upper: u8, oob: Option<u8>) -> Vec<u8> {
    let mut d = vec![u8::from(oob.is_some()) | (((ps - 1) as u8) << 1) | (((rs - 1) as u8) << 4)];
    d.extend(low.to_be_bytes());
    d.extend(high.to_be_bytes());
    let mut bw = BitWriter::default();
    for &(p, r) in lines {
        bw.bits(u64::from(p), ps);
        bw.bits(u64::from(r), rs);
    }
    bw.bits(u64::from(lower), ps);
    bw.bits(u64::from(upper), ps);
    if let Some(o) = oob {
        bw.bits(u64::from(o), ps);
    }
    d.extend(bw.bytes);
    d
}

#[test]
fn custom_huffman_tables_from_table_segments() {
    let syms = glyphs();
    let insts = instances(30, 90, 60, syms.len(), 31, false);
    // FS from -1024 to 1023 in two ranges; DS from -512 to 511 in two ranges and out of band.
    let fs_seg = table_segment(-1024, 1024, 3, 4, &[(2, 10), (2, 10)], 2, 2, None);
    let ds_seg = table_segment(-512, 512, 3, 4, &[(2, 9), (2, 9)], 3, 3, Some(2));
    let fs_t = Rc::new(Table::parse_segment(&fs_seg).expect("table"));
    let ds_t = Rc::new(Table::parse_segment(&ds_seg).expect("table"));
    let mut sel = HuffSel::standard();
    sel.fs = 3;
    sel.ds = 3;
    sel.custom[0] = Some(fs_t);
    sel.custom[1] = Some(ds_t);
    let o = TextOpts::plain();
    let data = stream(
        90,
        60,
        vec![
            segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()])),
            segment(2, 53, &[], &fs_seg),
            segment(3, 53, &[], &ds_seg),
            segment(4, 6, &[1, 2, 3], &text_region_huffman(90, 60, 0, 0, Op::Or, &o, &sel, &syms, &insts)),
        ],
    );
    let (got, warning) = run(&data, None, 90, 60);
    assert!(warning.is_none(), "{warning:?}");
    assert!(same(&expected_region(90, 60, &o, &syms, &insts), &got), "{}", show(&got));
    dump("huff_custom_tables", &data, None, 90, 60);
}

#[test]
fn custom_tables_one_at_a_time() {
    let syms = glyphs();
    let insts = instances(30, 90, 60, syms.len(), 31, false);
    let fs_seg = table_segment(-1024, 1024, 3, 4, &[(2, 10), (2, 10)], 2, 2, None);
    let ds_seg = table_segment(-512, 512, 3, 4, &[(2, 9), (2, 9)], 3, 3, Some(2));
    let fs_t = Rc::new(Table::parse_segment(&fs_seg).expect("table"));
    let ds_t = Rc::new(Table::parse_segment(&ds_seg).expect("table"));
    let dict = segment(1, 0, &[], &symbol_dict(&syms, 0, nominal_at(0), 0, &vec![true; syms.len()]));
    let o = TextOpts::plain();
    for which in 0..2 {
        let mut sel = HuffSel::standard();
        let (seg, refs) = if which == 0 {
            sel.fs = 3;
            sel.custom[0] = Some(fs_t.clone());
            (segment(2, 53, &[], &fs_seg), [1u32, 2])
        } else {
            sel.ds = 3;
            sel.custom[1] = Some(ds_t.clone());
            (segment(2, 53, &[], &ds_seg), [1, 2])
        };
        let data = stream(90, 60, vec![dict.clone(), seg, segment(3, 6, &refs, &text_region_huffman(90, 60, 0, 0, Op::Or, &o, &sel, &syms, &insts))]);
        dump(&format!("custom_one_{which}"), &data, None, 90, 60);
        let (got, warning) = run(&data, None, 90, 60);
        assert!(warning.is_none(), "{warning:?}");
        assert!(same(&expected_region(90, 60, &o, &syms, &insts), &got));
    }
}

#[test]
fn huffman_dictionaries_with_raw_and_mmr_bitmaps() {
    let mut syms = glyphs();
    syms.sort_by_key(|s| (s.h, s.w));
    // Heights must rise by at least 1 in table B.4, so make the classes distinct heights (they are 8, 10, 12).
    let export = vec![true; syms.len()];
    let insts = instances(40, 90, 60, syms.len(), 41, false);
    let o = TextOpts::plain();
    let seg_text = segment(2, 6, &[1], &text_region_huffman(90, 60, 0, 0, Op::Or, &o, &HuffSel::standard(), &syms, &insts));
    let data = stream(90, 60, vec![segment(1, 0, &[], &symbol_dict_huffman(&syms, &export, None)), seg_text.clone()]);
    let (got, warning) = run(&data, None, 90, 60);
    assert!(warning.is_none(), "{warning:?}");
    assert!(same(&expected_region(90, 60, &o, &syms, &insts), &got), "{}", show(&got));
    dump("huff_dict_raw", &data, None, 90, 60);
    // MMR coded collective bitmaps: coded by a small T.6 encoder.
    let data = stream(90, 60, vec![segment(1, 0, &[], &symbol_dict_huffman(&syms, &export, Some(&|b: &Bitmap| g4_encode(b)))), seg_text.clone()]);
    let (got, warning) = run(&data, None, 90, 60);
    assert!(warning.is_none(), "{warning:?}");
    assert!(same(&expected_region(90, 60, &o, &syms, &insts), &got), "{}", show(&got));
    dump("huff_dict_mmr", &data, None, 90, 60);
    // The end-of-block code is not needed when the size is told (6.2.6).
    let data = stream(90, 60, vec![segment(1, 0, &[], &symbol_dict_huffman(&syms, &export, Some(&|b: &Bitmap| g4_encode_with(b, false)))), seg_text]);
    let (got, warning) = run(&data, None, 90, 60);
    assert!(warning.is_none(), "{warning:?}");
    assert!(same(&expected_region(90, 60, &o, &syms, &insts), &got), "{}", show(&got));
    dump("huff_dict_mmr_noeofb", &data, None, 90, 60);
}

/// A T.6 (MMR) encoder, plain: every row against the one before, in pass, vertical and horizontal modes. 1 is black.
pub(super) fn g4_encode(b: &Bitmap) -> Vec<u8> {
    g4_encode_with(b, true)
}

fn g4_encode_with(b: &Bitmap, eofb: bool) -> Vec<u8> {
    let mut bw = BitWriter::default();
    let mut reference = vec![false; b.w];
    for y in 0..b.h {
        let row: Vec<bool> = (0..b.w).map(|x| b.get(x as i64, y as i64) == 1).collect();
        let changes = |r: &[bool]| -> Vec<usize> { (0..r.len()).filter(|&i| r[i] != if i == 0 { false } else { r[i - 1] }).collect() };
        let (cur, refc) = (changes(&row), changes(&reference));
        let w = b.w;
        let mut a0: i64 = -1;
        let mut black = false;
        while (a0 as usize) < w || a0 < 0 {
            // a1: the next change on the coding line right of a0; b1, b2 on the reference.
            let a1 = cur.iter().copied().find(|&c| c as i64 > a0 && (row[c]) != black).unwrap_or(w);
            let ref_color = |c: usize| reference[c];
            let b1 = refc.iter().copied().find(|&c| c as i64 > a0 && ref_color(c) != black).unwrap_or(w);
            let b2 = refc.iter().copied().find(|&c| c > b1).unwrap_or(w);
            if b2 < a1 {
                bw.bits(0b0001, 4); // pass
                a0 = b2 as i64;
            } else if (a1 as i64 - b1 as i64).abs() <= 3 {
                let d = a1 as i64 - b1 as i64;
                let (code, len): (u64, u32) = match d {
                    0 => (1, 1),
                    1 => (0b011, 3),
                    -1 => (0b010, 3),
                    2 => (0b000011, 6),
                    -2 => (0b000010, 6),
                    3 => (0b0000011, 7),
                    _ => (0b0000010, 7),
                };
                bw.bits(code, len);
                a0 = a1 as i64;
                black = !black;
            } else {
                let a2 = cur.iter().copied().find(|&c| c > a1).unwrap_or(w);
                bw.bits(0b001, 3);
                let start = a0.max(0) as usize;
                put_run(&mut bw, a1 - start, black);
                put_run(&mut bw, a2 - a1, !black);
                a0 = a2 as i64;
            }
            if a0 as usize >= w {
                break;
            }
        }
        reference = row;
    }
    if eofb {
        bw.bits(0x001001, 24);
    }
    bw.bytes
}

fn put_run(bw: &mut BitWriter, mut run: usize, black: bool) {
    // Terminating and make-up codes of T.4 (make-up codes for 64 to 1728 in steps of 64).
    while run >= 64 {
        let step = run.min(1728) / 64 * 64;
        let (code, len) = fax_code(step, black);
        bw.bits(code, len);
        run -= step;
    }
    let (code, len) = fax_code(run, black);
    bw.bits(code, len);
}

fn fax_code(run: usize, black: bool) -> (u64, u32) {
    // The code tables of the fax decoder are the ones of T.4; the decoder is checked against them elsewhere.
    crate::render::ccitt::test_code(run, black).expect("a code for the run")
}

#[test]
fn an_mmr_region_decodes_with_one_as_black() {
    // 16 by 4, the pixels 4 to 11 black in every row (coded by hand): row 0 horizontal mode (white 4, black 8) then a
    // vertical zero; the rows after it three vertical zeros each; then the end-of-block code.
    let mut bw = BitWriter::default();
    bw.bits(0b001, 3);
    bw.bits(0b1011, 4);
    bw.bits(0b000101, 6);
    bw.bit(1);
    for _ in 0..3 {
        bw.bits(0b111, 3);
    }
    bw.bits(0x001001, 24);
    let mut d = region_info(16, 4, 0, 0, Op::Or);
    d.push(1);
    d.extend(&bw.bytes);
    let data = stream(16, 4, vec![segment(1, 38, &[], &d)]);
    let (got, warning) = run(&data, None, 16, 4);
    assert!(warning.is_none(), "{warning:?}");
    let mut want = Bitmap::new(16, 4, false);
    for y in 0..4 {
        for x in 4..12 {
            want.set(x, y, 1);
        }
    }
    assert!(same(&want, &got), "{}", show(&got));
    // The encoder agrees with the hand-made stream, and its output for a picture decodes.
    assert_eq!(g4_encode(&want), bw.bytes);
    let pic = picture(61, 23, 3);
    let mut d = region_info(61, 23, 0, 0, Op::Or);
    d.push(1);
    d.extend(g4_encode(&pic));
    let data = stream(61, 23, vec![segment(1, 38, &[], &d)]);
    let (got, warning) = run(&data, None, 61, 23);
    assert!(warning.is_none(), "{warning:?}");
    assert!(same(&pic, &got), "{}", show(&got));
    dump("generic_mmr", &data, None, 61, 23);
}

#[test]
fn refinement_regions_of_an_intermediate_region_and_of_the_page() {
    let base = picture(70, 40, 51);
    let better = noisy(&base, 3, 40);
    for template in 0..2u8 {
        for tpgron in [false, true] {
            for at in [[(-1i8, -1i8), (-1, -1)], [(-2, -2), (3, 1)]] {
                // Intermediate generic region 1 (not drawn), refined by region 2 which refers to it.
                let refine_data = |reference: &Bitmap, x: i64, y: i64| {
                    let mut d = region_info(better.w, better.h, x, y, Op::Replace);
                    d.push(template | (u8::from(tpgron) << 1));
                    if template == 0 {
                        for a in at {
                            d.push(a.0 as u8);
                            d.push(a.1 as u8);
                        }
                    }
                    let mut mq = MqEnc::new();
                    let mut cx = vec![0u8; 1 << 13];
                    encode_refinement(&mut mq, &mut cx, &better, reference, 0, 0, template, tpgron, at);
                    d.extend(mq.finish());
                    d
                };
                let data = stream(
                    70,
                    40,
                    vec![segment(1, 36, &[], &generic_region(&base, 0, 0, Op::Or, 0, false, nominal_at(0))), segment(2, 42, &[1], &refine_data(&base, 0, 0))],
                );
                let (got, warning) = run(&data, None, 70, 40);
                assert!(warning.is_none(), "{warning:?}");
                assert!(same(&better, &got), "template {template} tpgron {tpgron} at {at:?}\n{}", show(&got));
                dump(&format!("refine_region_i_t{template}_p{}_{}", u8::from(tpgron), u8::from(at[0] != (-1, -1))), &data, None, 70, 40);
                // The page itself is the reference: draw `base`, then refine the part under the second region.
                let data = stream(
                    70,
                    40,
                    vec![segment(1, 38, &[], &generic_region(&base, 0, 0, Op::Or, 0, false, nominal_at(0))), segment(2, 42, &[], &refine_data(&base, 0, 0))],
                );
                let (got, warning) = run(&data, None, 70, 40);
                assert!(warning.is_none(), "{warning:?}");
                assert!(same(&better, &got), "page: template {template} tpgron {tpgron}");
                dump(&format!("refine_region_p_t{template}_p{}_{}", u8::from(tpgron), u8::from(at[0] != (-1, -1))), &data, None, 70, 40);
            }
        }
    }
}

#[test]
fn a_generic_region_whose_length_is_not_in_the_header() {
    let pic = picture(45, 31, 61);
    let mut d = generic_region(&pic, 2, 3, Op::Or, 0, false, nominal_at(0));
    d.extend(31u32.to_be_bytes());
    // The segment header with a data length of 0xFFFFFFFF (7.2.7), the end marker FF AC and the row count after it.
    let mut v = 1u32.to_be_bytes().to_vec();
    v.extend([38, 0, 1]);
    v.extend(u32::MAX.to_be_bytes());
    v.extend(d);
    let mut data = segment(0, 48, &[], &page_info(60, 40));
    data.extend(v);
    // Another segment after it, to be sure its end was found.
    let second = picture(10, 10, 62);
    data.extend(segment(2, 38, &[], &generic_region(&second, 40, 20, Op::Or, 1, true, nominal_at(1))));
    let (got, warning) = run(&data, None, 60, 40);
    assert!(warning.is_none(), "{warning:?}");
    let mut want = Bitmap::new(60, 40, false);
    want.combine(&pic, 2, 3, Op::Or);
    want.combine(&second, 40, 20, Op::Or);
    assert!(same(&want, &got), "{}", show(&got));
}

/// A pattern dictionary segment's data: `patterns` side by side, coded as a generic region.
fn pattern_dict(patterns: &[Bitmap], template: u8) -> Vec<u8> {
    pattern_dict_with(patterns, template, false)
}

fn pattern_dict_with(patterns: &[Bitmap], template: u8, mmr: bool) -> Vec<u8> {
    let (pw, ph) = (patterns[0].w, patterns[0].h);
    let mut sheet = Bitmap::new(pw * patterns.len(), ph, false);
    for (i, p) in patterns.iter().enumerate() {
        sheet.combine(p, (i * pw) as i64, 0, Op::Replace);
    }
    let mut d = vec![(template << 1) | u8::from(mmr), pw as u8, ph as u8];
    d.extend(((patterns.len() - 1) as u32).to_be_bytes());
    if mmr {
        d.extend(g4_encode(&sheet));
        return d;
    }
    let at = [(-(pw as i8), 0), (-3, -1), (2, -2), (-2, -2)];
    let mut mq = MqEnc::new();
    let mut cx = vec![0u8; 1 << 16];
    encode_generic(&mut mq, &mut cx, &sheet, template, false, at, None);
    d.extend(mq.finish());
    d
}

struct Grid {
    gw: usize,
    gh: usize,
    gx: i32,
    gy: i32,
    rx: u16,
    ry: u16,
}

fn cell(g: &Grid, m: usize, n: usize) -> (i64, i64) {
    let (m, n) = (m as i64, n as i64);
    ((i64::from(g.gx) + m * i64::from(g.ry) + n * i64::from(g.rx)) >> 8, (i64::from(g.gy) + m * i64::from(g.rx) - n * i64::from(g.ry)) >> 8)
}

/// A halftone region segment's data and the picture it should make.
fn halftone(region: (usize, usize), g: &Grid, patterns: &[Bitmap], template: u8, skip: bool, seed: u32) -> (Vec<u8>, Bitmap) {
    halftone_with(region, g, patterns, template, skip, seed, false)
}

fn halftone_with(region: (usize, usize), g: &Grid, patterns: &[Bitmap], template: u8, skip: bool, seed: u32, mmr: bool) -> (Vec<u8>, Bitmap) {
    let bpp = ceil_log2(patterns.len());
    let (pw, ph) = (patterns[0].w as i64, patterns[0].h as i64);
    let mut s = seed;
    let mut skipmap = Bitmap::new(g.gw, g.gh, false);
    let mut gray = vec![0usize; g.gw * g.gh];
    for m in 0..g.gh {
        for n in 0..g.gw {
            let (x, y) = cell(g, m, n);
            let skipped = skip && (x + pw <= 0 || x >= region.0 as i64 || y + ph <= 0 || y >= region.1 as i64);
            if skipped {
                skipmap.set(n, m, 1);
            }
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            gray[m * g.gw + n] = if skipped { 0 } else { (s >> 10) as usize % patterns.len() };
        }
    }
    let mut want = Bitmap::new(region.0, region.1, false);
    for m in 0..g.gh {
        for n in 0..g.gw {
            let (x, y) = cell(g, m, n);
            want.combine(&patterns[gray[m * g.gw + n]], x, y, Op::Or);
        }
    }
    let mut d = region_info(region.0, region.1, 0, 0, Op::Or);
    d.push(u8::from(mmr) | (template << 1) | (u8::from(skip) << 3));
    d.extend((g.gw as u32).to_be_bytes());
    d.extend((g.gh as u32).to_be_bytes());
    d.extend(g.gx.to_be_bytes());
    d.extend(g.gy.to_be_bytes());
    d.extend(g.rx.to_be_bytes());
    d.extend(g.ry.to_be_bytes());
    let mut mq = MqEnc::new();
    let mut cx = vec![0u8; 1 << 16];
    let at = [(if template <= 1 { 3 } else { 2 }, -1), (-3, -1), (2, -2), (-2, -2)];
    let mut prev: Option<Bitmap> = None;
    let mut planes: Vec<u8> = Vec::new();
    for j in (0..bpp).rev() {
        // The plane of bit j, then its Gray code against the plane above it.
        let mut plane = Bitmap::new(g.gw, g.gh, false);
        for m in 0..g.gh {
            for n in 0..g.gw {
                plane.set(n, m, ((gray[m * g.gw + n] >> j) & 1) as u8);
            }
        }
        let mut coded = Bitmap::new(g.gw, g.gh, false);
        coded.combine(&plane, 0, 0, Op::Replace);
        if let Some(p) = &prev {
            coded.combine(p, 0, 0, Op::Xor);
        }
        if mmr {
            // Each plane is a bitmap of its own, with its end-of-block code.
            planes.extend(g4_encode(&coded));
        } else {
            encode_generic(&mut mq, &mut cx, &coded, template, false, at, skip.then_some(&skipmap));
        }
        prev = Some(plane);
    }
    d.extend(if mmr { planes } else { mq.finish() });
    (d, want)
}

#[test]
fn halftone_regions_round_trip() {
    let patterns: Vec<Bitmap> = (0..11).map(|i| picture(4, 4, 80 + i)).collect();
    for template in 0..4u8 {
        for (grid, skip) in [
            (Grid { gw: 14, gh: 10, gx: -2 << 8, gy: -3 << 8, rx: 4 << 8, ry: 0 }, false),
            (Grid { gw: 20, gh: 20, gx: 10 << 8, gy: 0, rx: 1000, ry: 230 }, false),
            (Grid { gw: 22, gh: 18, gx: -(5 << 8), gy: 5 << 8, rx: 1000, ry: 230 }, true),
        ] {
            let (hd, want) = halftone((64, 48), &grid, &patterns, template, skip, 7);
            let data = stream(64, 48, vec![segment(1, 16, &[], &pattern_dict(&patterns, template)), segment(2, 22, &[1], &hd)]);
            let (got, warning) = run(&data, None, 64, 48);
            assert!(warning.is_none(), "{warning:?}");
            assert!(same(&want, &got), "template {template} skip {skip}\n{}", show(&got));
            dump(&format!("halftone_t{template}_g{}_{}", grid.rx, u8::from(skip)), &data, None, 64, 48);
        }
    }
    // MMR coded patterns and planes (no skipping with MMR: the planes are coded as they are).
    for (grid, name) in [(Grid { gw: 14, gh: 10, gx: -2 << 8, gy: -3 << 8, rx: 4 << 8, ry: 0 }, "a"), (Grid { gw: 20, gh: 20, gx: 10 << 8, gy: 0, rx: 1000, ry: 230 }, "b")] {
        let (hd, want) = halftone_with((64, 48), &grid, &patterns, 0, false, 9, true);
        let data = stream(64, 48, vec![segment(1, 16, &[], &pattern_dict_with(&patterns, 0, true)), segment(2, 22, &[1], &hd)]);
        dump(&format!("halftone_mmr_{name}"), &data, None, 64, 48);
        let (got, warning) = run(&data, None, 64, 48);
        assert!(warning.is_none(), "{warning:?}");
        assert!(same(&want, &got), "mmr {name}
{}", show(&got));
    }
    // One pattern: no bit planes at all.
    let one = vec![picture(5, 5, 99)];
    let grid = Grid { gw: 8, gh: 6, gx: 0, gy: 0, rx: 5 << 8, ry: 0 };
    let (hd, want) = halftone((40, 30), &grid, &one, 0, false, 1);
    let data = stream(40, 30, vec![segment(1, 16, &[], &pattern_dict(&one, 0)), segment(2, 22, &[1], &hd)]);
    let (got, _) = run(&data, None, 40, 30);
    assert!(same(&want, &got));
}
