//! Streams built to hurt the decoder, the kinds earlier reviews found in the other decoders: counts in a header that
//! are far more than the data can hold, sizes that need gigabytes, levels and layers past what is allowed, data that
//! stops or is 0xFF for ever, tables that lie. The rule: no panic, no hang, memory held in check, and the page gets
//! what could be decoded (with a warning) or the work meter says the page has had enough.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop)]

use std::time::{Duration, Instant};

use super::test_enc::{Cfg, Poc, encode, jp2, jp2_box};
use super::{Alpha, Decoded, GROUP_BYTES_TEST, MAX_MEMORY, Options, Warnings, decode};
use crate::error::{Error, Result};
use crate::render::work::{PAGE_WORK, Work};

const SECS: u64 = 20;

fn opts(want: (usize, usize)) -> Options {
    Options { want, components: None, indexed: false, alpha: Alpha::Ignore, threads: None }
}

/// Decode, and say how much work it was and how long it took.
fn go(data: &[u8], want: (usize, usize)) -> (Result<Decoded>, f64, Duration) {
    let work = Work::new();
    let start = Instant::now();
    let r = decode(data, &opts(want), &work);
    let took = start.elapsed();
    assert!(took < Duration::from_secs(SECS), "took {took:?}");
    if std::env::var("JPX_VERBOSE").is_ok() {
        eprintln!("{} bytes: {} in {took:?}, {:.0} units", data.len(), r.as_ref().map_or_else(|e| format!("{e}"), |d| format!("{}x{}", d.w, d.h)), work.used());
    }
    (r, work.used(), took)
}

fn is_limit(r: &Result<Decoded>) -> bool {
    matches!(r, Err(Error::Limit(_)))
}

fn is_refused(r: &Result<Decoded>) -> bool {
    matches!(r, Err(Error::Limit(_) | Error::Invalid(_) | Error::Unsupported(_)))
}

fn put16(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&(x as u16).to_be_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn marker(out: &mut Vec<u8>, code: u16, body: &[u8]) {
    out.extend_from_slice(&code.to_be_bytes());
    put16(out, body.len() as u32 + 2);
    out.extend_from_slice(body);
}

/// SIZ for an image of `w` by `h` with tiles of `tw` by `th` and the given components (depth, dx, dy).
fn siz(w: u32, h: u32, tw: u32, th: u32, comps: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut b = Vec::new();
    put16(&mut b, 0);
    for v in [w, h, 0, 0, tw, th, 0, 0] {
        put32(&mut b, v);
    }
    put16(&mut b, comps.len() as u32);
    for &(d, dx, dy) in comps {
        b.extend_from_slice(&[d - 1, dx, dy]);
    }
    b
}

/// A codestream from its pieces: COD with the given levels, layers, code-block exponents and style, a reversible
/// QCD, then `tiles` (SOT segments and so on) as they are.
fn stream(siz: &[u8], levels: u8, layers: u16, cb: (u8, u8), style: u8, precincts: Option<&[u8]>, tiles: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0x4F];
    marker(&mut out, 0xFF51, siz);
    let mut cod = vec![u8::from(precincts.is_some()), 0];
    put16(&mut cod, u32::from(layers));
    cod.extend_from_slice(&[0, levels, cb.0 - 2, cb.1 - 2, style, 1]);
    if let Some(p) = precincts {
        cod.extend_from_slice(p);
    }
    marker(&mut out, 0xFF52, &cod);
    let mut qcd = vec![2 << 5];
    qcd.extend((0..3 * usize::from(levels) + 1).map(|_| 10u8 << 3));
    marker(&mut out, 0xFF5C, &qcd);
    out.extend_from_slice(tiles);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// A tile-part for tile `t` with `body` as its packets.
fn tile_part(t: u32, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0x90, 0, 10];
    put16(&mut v, t);
    put32(&mut v, 14 + body.len() as u32);
    v.extend_from_slice(&[0, 1, 0xFF, 0x93]);
    v.extend_from_slice(body);
    v
}

#[test]
fn a_65535_square_image_of_tiles_of_size_one() {
    // 4 billion tiles in the header, a few bytes of data: refused before anything is made.
    let s = stream(&siz(65535, 65535, 1, 1, &[(8, 1, 1)]), 0, 1, (4, 4), 0, None, &tile_part(0, &[0x80]));
    let (r, used, took) = go(&s, (10, 10));
    assert!(is_limit(&r), "{:?}", r.err());
    assert!(used < 1e6 && took < Duration::from_secs(1));
    // The most tiles the standard allows, each of a pixel or two, and a lot of tile-parts for them.
    let mut tiles = Vec::new();
    for t in 0..65_000u32 {
        tiles.extend(tile_part(t, &[0x00]));
    }
    let s = stream(&siz(255, 257, 1, 1, &[(8, 1, 1), (8, 1, 1), (8, 1, 1)]), 5, 1, (4, 4), 0, None, &tiles);
    let (r, used, _) = go(&s, (0, 0));
    // Done (the tiles say nothing) or stopped by the meter, but in time and within the page's allowance.
    assert!(r.is_ok() || is_refused(&r), "{:?}", r.err());
    assert!(used <= PAGE_WORK);
}

#[test]
fn too_many_decomposition_levels() {
    let s = stream(&siz(64, 64, 64, 64, &[(8, 1, 1)]), 33, 1, (4, 4), 0, None, &tile_part(0, &[0x00]));
    let (r, _, _) = go(&s, (0, 0));
    assert!(is_refused(&r) && !is_limit(&r), "{:?}", r.err());
    // 32 levels on a tile that is 5 pixels wide: all the resolutions but the top are empty.
    let s = stream(&siz(5, 5, 5, 5, &[(8, 1, 1)]), 32, 1, (4, 4), 0, None, &tile_part(0, &[0x00]));
    let (r, _, _) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
}

#[test]
fn a_code_block_with_a_huge_pass_count() {
    // One packet: not empty, the block included, zero bit-planes 0, 164 passes (the largest the header can say), a
    // length of 0 bytes. The block is told to have far more passes than a coefficient has bit-planes.
    let mut bits = String::from("1");
    bits.push('1'); // included (a one-leaf tag tree: the value 0 is told with a 1 after no zeros: "1")
    bits.push('1'); // zero bit-planes: 0 (known to be below 1)
    bits.push_str("111111111"); // 1111 11111
    bits.push_str("1111111"); // 164 - 37 = 127
    bits.push('0'); // Lblock stays 3
    bits.push_str(&"0".repeat(3 + 7)); // length 0 in 10 bits
    while bits.len() % 8 != 0 {
        bits.push('0');
    }
    let body: Vec<u8> = bits.as_bytes().chunks(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).expect("utf8"), 2).expect("bits")).collect();
    let s = stream(&siz(16, 16, 16, 16, &[(8, 1, 1)]), 0, 1, (4, 4), 0, None, &tile_part(0, &body));
    let (r, used, _) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
    assert!(used < 1e8, "{used}");
    // A block that is allowed all its passes and has no data: the arithmetic decoder runs dry and stops early.
    let mut bits = String::from("1");
    bits.push_str("111"); // included, zero bit-planes 0
    bits.push_str("1111"); // 1111, then 5 bits: 6 + 30 = 36 passes
    bits.push_str("01111");
    bits.push('0');
    bits.push_str(&"0".repeat(3 + 5));
    while bits.len() % 8 != 0 {
        bits.push('0');
    }
    let body: Vec<u8> = bits.as_bytes().chunks(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).expect("utf8"), 2).expect("bits")).collect();
    let s = stream(&siz(64, 64, 64, 64, &[(8, 1, 1)]), 0, 1, (6, 6), 0, None, &tile_part(0, &body));
    let (r, used, took) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
    assert!(used < 5e7 && took < Duration::from_secs(5));
}

#[test]
fn a_precinct_of_size_two_to_the_fifteenth() {
    let mut cfg = Cfg::new(70, 53, 1);
    cfg.levels = 3;
    cfg.precincts = Some(vec![(15, 15); 4]);
    cfg.cb = (6, 6);
    let (r, _, _) = go(&encode(&cfg).codestream, (0, 0));
    assert!(r.is_ok());
    // On a tile 65535 wide, with code-blocks of 4 by 4 inside precincts of 1 by 1 at the lowest resolution: far too
    // many precincts and code-blocks for the data.
    let s = stream(&siz(65535, 16, 65535, 16, &[(8, 1, 1)]), 0, 1, (2, 2), 0, Some(&[0x00]), &tile_part(0, &[0x00; 8]));
    let (r, used, took) = go(&s, (0, 0));
    assert!(is_limit(&r), "{:?}", r.err());
    assert!(used < 5e8 && took < Duration::from_secs(5));
    // Precincts of 1 by 1 pixel at the lowest resolution of an image that is 600 wide with 5 levels.
    let s = stream(&siz(2000, 2000, 2000, 2000, &[(8, 1, 1)]), 0, 1, (2, 2), 0, Some(&[0x00]), &tile_part(0, &[0x00; 8]));
    let (r, _, _) = go(&s, (0, 0));
    assert!(is_limit(&r), "{:?}", r.err());
}

#[test]
fn a_giant_image_needs_levels_to_be_decoded_small() {
    // 100000 by 100000 with no levels would need gigabytes; with levels it is decoded small.
    let s = stream(&siz(100_000, 100_000, 100_000, 100_000, &[(8, 1, 1)]), 0, 1, (6, 6), 0, None, &tile_part(0, &[0x00]));
    let (r, used, took) = go(&s, (10, 10));
    assert!(is_limit(&r), "{:?}", r.err());
    assert!(used < 1e8 && took < Duration::from_secs(2));
    let s = stream(&siz(100_000, 100_000, 100_000, 100_000, &[(8, 1, 1)]), 12, 1, (6, 6), 0, Some(&[0xFF; 13]), &tile_part(0, &[0x00]));
    let (r, _, _) = go(&s, (50, 50));
    assert!(r.is_ok() || is_refused(&r), "{:?}", r.err());
}

#[test]
fn truncated_streams() {
    let mut cfg = Cfg::new(61, 47, 3);
    cfg.levels = 3;
    cfg.layers = 2;
    cfg.cb = (3, 3);
    cfg.mct = true;
    cfg.tile = (40, 32);
    let full = encode(&cfg).codestream;
    let (r, _, _) = go(&full, (0, 0));
    assert!(r.is_ok());
    // Every prefix of the stream (every byte of the headers, then every few).
    let mut tried = 0;
    let mut n = 0;
    while n < full.len() {
        let (r, _, _) = go(&full[..n], (0, 0));
        assert!(r.is_ok() || is_refused(&r));
        n += if n < 140 { 1 } else { 7 };
        tried += 1;
    }
    assert!(tried > 200);
    // The same as a JP2 file cut short.
    let file = jp2(&cfg, &full, 16, &[]);
    for n in (0..file.len()).step_by(11) {
        let (r, _, _) = go(&file[..n], (0, 0));
        assert!(r.is_ok() || is_refused(&r));
    }
    // A stream cut in the middle of the data still gives a picture, with a warning.
    let (r, _, _) = go(&full[..full.len() * 2 / 3], (0, 0));
    let d = r.expect("picture");
    assert!(d.warning.is_some());
}

#[test]
fn data_that_is_all_ones() {
    let mut cfg = Cfg::new(61, 47, 3);
    cfg.levels = 3;
    cfg.layers = 2;
    cfg.cb = (3, 3);
    let full = encode(&cfg).codestream;
    // Everything after the main header is 0xFF.
    let sot = full.windows(2).position(|w| w == [0xFF, 0x90]).expect("a tile-part");
    let mut s = full.clone();
    for b in s.iter_mut().skip(sot + 14) {
        *b = 0xFF;
    }
    let (r, used, _) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
    assert!(used <= PAGE_WORK);
    // Every byte of the tile data in turn is 0xFF, 0x00 or flipped: no panic.
    for at in (sot + 14..full.len() - 2).step_by(23) {
        for v in [0xFFu8, 0x00, !full[at]] {
            let mut s = full.clone();
            s[at] = v;
            let (r, _, _) = go(&s, (0, 0));
            assert!(r.is_ok() || is_refused(&r));
        }
    }
    // And a whole stream of 0xFF after SOC.
    let mut s = vec![0xFF, 0x4F];
    s.extend([0xFF; 4000]);
    let (r, _, _) = go(&s, (0, 0));
    assert!(is_refused(&r));
}

#[test]
fn sixteen_thousand_components() {
    let comps = vec![(8u8, 1u8, 1u8); 16384];
    let s = stream(&siz(16, 16, 16, 16, &comps), 1, 1, (4, 4), 0, None, &tile_part(0, &[0x00]));
    let (r, used, _) = go(&s, (0, 0));
    assert!(is_limit(&r), "{:?}", r.err());
    assert!(used < 1e6);
    // The most there is room for here: it is decoded, or stopped, in time.
    let comps = vec![(8u8, 1u8, 1u8); 256];
    let s = stream(&siz(16, 16, 16, 16, &comps), 1, 1, (4, 4), 0, None, &tile_part(0, &[0x00; 40]));
    let (r, used, _) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
    assert!(used < PAGE_WORK);
}

#[test]
fn a_palette_with_a_huge_entry_count() {
    let mut cfg = Cfg::new(20, 20, 1);
    cfg.cb = (3, 3);
    let es = encode(&cfg).codestream;
    // 65535 entries of 255 columns of 16 bits in a box of a few bytes.
    let mut pclr = 65535u16.to_be_bytes().to_vec();
    pclr.push(255);
    pclr.extend([15u8; 255]);
    pclr.extend([0u8; 40]);
    let cmap: Vec<u8> = (0..255u16).flat_map(|i| [0, 0, 1, i as u8]).collect();
    let file = jp2(&cfg, &es, 16, &[jp2_box(b"pclr", &pclr), jp2_box(b"cmap", &cmap)]);
    let (r, used, _) = go(&file, (0, 0));
    // The palette is not used (it does not add up); the grey picture is.
    let d = r.expect("picture");
    assert_eq!(d.ncomp, 1);
    assert!(used < 1e8);
    // The largest palette the standard has, in a box that holds it, is used.
    let mut pclr = 1024u16.to_be_bytes().to_vec();
    pclr.extend_from_slice(&[3, 7, 7, 7]);
    pclr.extend((0..1024 * 3).map(|i| (i % 251) as u8));
    let cmap = [0u8, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2];
    let file = jp2(&cfg, &es, 16, &[jp2_box(b"pclr", &pclr), jp2_box(b"cmap", &cmap)]);
    let (r, _, _) = go(&file, (0, 0));
    assert_eq!(r.expect("picture").ncomp, 3);
    // More entries than the standard allows: not used.
    let mut pclr = 1025u16.to_be_bytes().to_vec();
    pclr.extend_from_slice(&[1, 7]);
    pclr.extend((0..1025).map(|i| (i % 251) as u8));
    let file = jp2(&cfg, &es, 16, &[jp2_box(b"pclr", &pclr), jp2_box(b"cmap", &[0, 0, 1, 0])]);
    let (r, _, _) = go(&file, (0, 0));
    assert_eq!(r.expect("picture").ncomp, 1);
    // Boxes: far too many of them, and a box that says it is longer than the file.
    let mut file = jp2(&cfg, &es, 16, &[]);
    let at = file.windows(4).position(|w| w == b"jp2h").expect("box") - 4;
    file[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    let (r, _, _) = go(&file, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
    let mut many = jp2(&cfg, &es, 16, &[]);
    many.truncate(many.windows(4).position(|w| w == b"jp2c").expect("box") - 4);
    for _ in 0..20_000 {
        many.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    }
    let (r, used, _) = go(&many, (0, 0));
    assert!(is_refused(&r), "{:?}", r.err());
    assert!(used < 1e8);
}

#[test]
fn packed_headers_that_lie_or_run_out() {
    let mut cfg = Cfg::new(70, 53, 3);
    cfg.levels = 3;
    cfg.layers = 3;
    cfg.cb = (3, 3);
    cfg.tile = (40, 32);
    cfg.tile_parts = 2;
    let reference = decode(&encode(&cfg).codestream, &opts((0, 0)), &Work::new()).expect("decodes");
    for (ppm, ppt) in [(true, false), (false, true)] {
        cfg.ppm = ppm;
        cfg.ppt = ppt;
        let good = encode(&cfg).codestream;
        let d = decode(&good, &opts((0, 0)), &Work::new()).expect("decodes");
        assert!(d.data == reference.data && d.warning.is_none());
        let marker_at = good.windows(2).position(|w| w == if ppm { [0xFF, 0x60] } else { [0xFF, 0x61] }).expect("marker");
        // The packed headers cut short (the marker says so, the data after it is the tile's data): damage is reported.
        let mut cut = good.clone();
        let len = u16::from_be_bytes([cut[marker_at + 2], cut[marker_at + 3]]);
        cut[marker_at + 2..marker_at + 4].copy_from_slice(&(len / 3).max(3).to_be_bytes());
        let (r, used, _) = go(&cut, (0, 0));
        assert!(r.is_ok() || is_refused(&r));
        assert!(used < PAGE_WORK);
        // The first block's length (PPM) or the marker's index repeated: no panic.
        let mut lie = good.clone();
        if ppm {
            lie[marker_at + 5..marker_at + 9].copy_from_slice(&u32::MAX.to_be_bytes());
        } else {
            lie[marker_at + 4] = 7;
        }
        let (r, _, _) = go(&lie, (0, 0));
        assert!(r.is_ok() || is_refused(&r));
        // The same marker over and over: the headers are used once, not again for every tile-part.
        let end = marker_at + 2 + usize::from(u16::from_be_bytes([good[marker_at + 2], good[marker_at + 3]]));
        let mut many = good[..end].to_vec();
        for _ in 0..50 {
            many.extend_from_slice(&good[marker_at..end]);
        }
        many.extend_from_slice(&good[end..]);
        let (r, _, _) = go(&many, (0, 0));
        assert!(r.is_ok() || is_refused(&r));
    }
}

#[test]
fn progression_order_changes_with_huge_ranges() {
    let mut cfg = Cfg::new(70, 53, 3);
    cfg.levels = 3;
    cfg.layers = 4;
    cfg.cb = (3, 3);
    cfg.precincts = Some(vec![(3, 3); 4]);
    // Thousands of changes each reaching everything (and a few that reach past it): each packet is had once, and
    // every step of the loops is charged.
    let wide = Poc { rs: 0, cs: 0, lye: 65535, re: 255, ce: 16383, prog: 0 };
    cfg.poc = (0..4000).map(|i| Poc { prog: (i % 5) as u8, ..wide.clone() }).collect();
    let (r, used, took) = go(&encode(&cfg).codestream, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
    assert!(used <= PAGE_WORK && took < Duration::from_secs(SECS));
    // The same with a meter that is nearly empty: it stops at once.
    let work = Work::with_allowance(2e5);
    let r = decode(&encode(&cfg).codestream, &opts((0, 0)), &work);
    assert!(is_limit(&r), "{:?}", r.err());
    // A marker with more changes than the cap.
    let mut s = encode(&Cfg::new(8, 8, 1)).codestream;
    let at = s.windows(2).position(|w| w == [0xFF, 0x5C]).expect("QCD");
    let mut poc = Vec::new();
    for _ in 0..9000 {
        poc.extend_from_slice(&[0, 0, 0, 1, 1, 1, 0]);
    }
    let mut seg = vec![0xFF, 0x5F];
    seg.extend_from_slice(&((poc.len().min(65_000) + 2) as u16).to_be_bytes());
    seg.extend_from_slice(&poc[..poc.len().min(65_000) / 7 * 7]);
    s.splice(at..at, seg);
    let (r, _, _) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
}

#[test]
fn the_meter_stops_a_decode_and_discarded_levels_are_not_worked() {
    let mut cfg = Cfg::new(160, 120, 3);
    cfg.levels = 4;
    cfg.cb = (4, 4);
    cfg.mct = true;
    cfg.layers = 2;
    let s = encode(&cfg).codestream;
    let (full, used_full, _) = go(&s, (160, 120));
    let d = full.expect("picture");
    assert_eq!((d.w, d.h), (160, 120));
    for (want, divisor) in [((80, 60), 3.0), ((40, 30), 6.0), ((20, 15), 9.0)] {
        let (r, used, _) = go(&s, want);
        let d = r.expect("picture");
        assert_eq!((d.w, d.h), want);
        assert!(used * divisor < used_full, "{want:?}: {used} against {used_full}");
    }
    // A meter that has little left refuses the decode (and says the page is over).
    let work = Work::with_allowance(used_full / 10.0);
    assert!(is_limit(&decode(&s, &opts((160, 120)), &work)));
    assert!(work.is_over());
    // Not decoded at all once it is over.
    let before = work.used();
    assert!(is_limit(&decode(&s, &opts((160, 120)), &work)));
    assert!((work.used() - before).abs() < 1.0);
}

#[test]
fn memory_is_given_back_tile_by_tile() {
    // Hundreds of tiles: what each took for its structures and buffers is given back when it is done, so that what
    // is left at the end is the planes and the pixels only.
    let mut cfg = Cfg::new(120, 90, 3);
    cfg.levels = 2;
    cfg.cb = (3, 3);
    cfg.tile = (8, 6);
    cfg.mct = true;
    let d = decode(&encode(&cfg).codestream, &opts((0, 0)), &Work::new()).expect("decodes");
    let (planes, pixels, tiles) = (3 * 120 * 90, 3 * 120 * 90, 15 * 15);
    // (What the file's table of tiles takes stays taken until the decode is done: a few hundred bytes a tile.)
    let held = MAX_MEMORY - planes - pixels - d.memory_left;
    assert!(held <= 600 * tiles as u64, "{held} bytes still held");
    // The same with tile-parts, layers, packed headers and a reduction.
    cfg.layers = 3;
    cfg.tile_parts = 2;
    cfg.ppt = true;
    let d = decode(&encode(&cfg).codestream, &opts((60, 45)), &Work::new()).expect("decodes");
    assert_eq!((d.w, d.h), (60, 45));
    let held = MAX_MEMORY - 2 * 3 * 60 * 45 - d.memory_left;
    assert!(held <= 800 * tiles as u64, "{held} bytes still held");
}

#[test]
fn more_code_blocks_than_allowed() {
    // 8192 by 8192 in code-blocks of 4 by 4: 4 million of them.
    let s = stream(&siz(8192, 8192, 8192, 8192, &[(8, 1, 1)]), 0, 1, (2, 2), 0, None, &tile_part(0, &[0x00; 8]));
    let (r, used, took) = go(&s, (0, 0));
    assert!(is_limit(&r), "{:?}", r.err());
    assert!(used < 5e8 && took < Duration::from_secs(5));
    // Layers times precincts: 65535 layers of a tile with 400 precincts.
    let s = stream(&siz(640, 640, 640, 640, &[(8, 1, 1)]), 0, 65535, (6, 6), 0, Some(&[0x33]), &tile_part(0, &[0x00; 8]));
    let (r, _, _) = go(&s, (0, 0));
    assert!(r.is_ok() || is_refused(&r));
}

#[test]
fn the_meter_stops_decoding_that_runs_on_several_threads() {
    let mut cfg = Cfg::new(340, 300, 3);
    cfg.levels = 3;
    cfg.cb = (5, 5);
    cfg.layers = 2;
    let s = encode(&cfg).codestream;
    let o = Options { threads: Some(4), ..opts((0, 0)) };
    let full = Work::new();
    decode(&s, &o, &full).expect("decodes");
    for share in [0.9, 0.5, 0.2, 0.05] {
        let work = Work::with_allowance(full.used() * share);
        let start = Instant::now();
        let r = decode(&s, &o, &work);
        assert!(is_limit(&r), "{share}: {:?}", r.err());
        assert!(work.is_over() && start.elapsed() < Duration::from_secs(5));
    }
}

// --- found by the review of 3c2-2 ---------------------------------------------------------------------------------

#[test]
fn a_channel_definition_that_names_65534_channels() {
    // One 16-bit component named as the colour of every channel the box can name: a table of 65536 entries for each
    // is 4 GB. The colour space has one channel (and so the picture): what is named past it is not used.
    let mut cfg = Cfg::new(20, 20, 1);
    cfg.comps[0].depth = 16;
    cfg.cb = (3, 3);
    let es = encode(&cfg).codestream;
    let n = 65534usize;
    let mut cdef = (n as u16).to_be_bytes().to_vec();
    for i in 0..n {
        cdef.extend_from_slice(&[0, 0, 0, 0]);
        cdef.extend_from_slice(&((i + 1) as u16).to_be_bytes());
    }
    let file = jp2(&cfg, &es, 17, &[jp2_box(b"cdef", &cdef)]);
    let (r, used, _) = go(&file, (0, 0));
    let d = r.expect("picture");
    assert_eq!(d.ncomp, 1);
    assert!(d.warning.as_deref().is_some_and(|w| w.contains("colour channels")), "{:?}", d.warning);
    assert!(used < 1e8, "{used}");
    // Its one table (64 KB) is all it took besides the planes and the pixels.
    assert!(MAX_MEMORY - d.memory_low < 1 << 20, "{} bytes", MAX_MEMORY - d.memory_low);
    // Four colours and an opacity, as many as there can be, each named twice: tables for those.
    let mut cfg = Cfg::new(20, 20, 5);
    cfg.cb = (3, 3);
    let es = encode(&cfg).codestream;
    let mut cdef = 40u16.to_be_bytes().to_vec();
    for i in 0..40u16 {
        let (typ, asoc) = if i % 5 == 4 { (1u16, 0u16) } else { (0, i % 5 + 1) };
        cdef.extend_from_slice(&(i % 5).to_be_bytes());
        cdef.extend_from_slice(&typ.to_be_bytes());
        cdef.extend_from_slice(&asoc.to_be_bytes());
    }
    let file = jp2(&cfg, &es, 12, &[jp2_box(b"cdef", &cdef)]);
    let work = Work::new();
    let o = Options { alpha: Alpha::Straight, ..opts((0, 0)) };
    let d = decode(&file, &o, &work).expect("picture");
    assert_eq!(d.ncomp, 4);
    assert!(d.alpha.is_some());
}

#[test]
fn warnings_are_kept_to_a_few() {
    let mut w = Warnings::default();
    for i in 0..100 {
        w.push(format!("w{i}"));
        w.push("w0".to_string());
    }
    let t = w.text().expect("text");
    assert!(t.starts_with("w0; w1; "), "{t}");
    assert!(t.contains("w7") && !t.contains("w8"), "{t}");
    assert!(t.ends_with("and 92 more"), "{t}");
    // A file of thousands of tile-parts of a tile it does not have says so once.
    let mut tiles = tile_part(0, &[0x00; 4]);
    for _ in 0..5000 {
        tiles.extend(tile_part(7, &[0]));
    }
    let s = stream(&siz(16, 16, 16, 16, &[(8, 1, 1)]), 1, 1, (4, 4), 0, None, &tiles);
    let (r, _, _) = go(&s, (0, 0));
    let text = r.expect("picture").warning.expect("a warning");
    assert_eq!(text.matches("does not exist").count(), 1, "{text}");
    assert!(text.len() < 400, "{} bytes", text.len());
    // Hundreds of tiles each damaged in its own way (every tile has a different number of damaged code-blocks).
    let mut cfg = Cfg::new(120, 90, 1);
    cfg.levels = 2;
    cfg.cb = (3, 3);
    cfg.tile = (8, 6);
    let mut s = encode(&cfg).codestream;
    for (i, b) in s.iter_mut().enumerate().skip(200) {
        if i % 7 == 0 {
            *b = 0xFF;
        }
    }
    let (r, _, _) = go(&s, (0, 0));
    if let Ok(d) = r {
        assert!(d.warning.as_deref().map_or(0, str::len) < 1200, "{:?}", d.warning);
    }
}

#[test]
fn empty_ranges_of_progression_order_changes_are_charged() {
    // 65535 layers, an entry that names components from 2 up to 1: the loops over layers and resolutions run, and
    // nothing is in them. They are charged all the same, whatever the optimizer makes of them.
    let s = stream(&siz(16, 16, 16, 16, &[(8, 1, 1), (8, 1, 1), (8, 1, 1)]), 1, 65535, (4, 4), 0, None, &tile_part(0, &[0x00; 8]));
    let at = s.windows(2).position(|w| w == [0xFF, 0x5C]).expect("QCD");
    for prog in [0u8, 1] {
        let mut changed = s.clone();
        let mut seg = vec![0xFF, 0x5F, 0, 9];
        seg.extend_from_slice(&[0, 2, 0xFF, 0xFF, 255, 1, prog]);
        changed.splice(at..at, seg);
        let (r, used, _) = go(&changed, (0, 0));
        assert!(r.is_ok() || is_refused(&r));
        assert!(used > 1.5e7, "order {prog}: {used}");
    }
}

/// A tile-part of tile `t` with `extra` marker segments in its header.
fn tile_part_with(t: u32, extra: &[u8], body: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0x90, 0, 10];
    put16(&mut v, t);
    put32(&mut v, 14 + (extra.len() + body.len()) as u32);
    v.extend_from_slice(&[0, 1]);
    v.extend_from_slice(extra);
    v.extend_from_slice(&[0xFF, 0x93]);
    v.extend_from_slice(body);
    v
}

#[test]
fn the_coding_parameters_of_every_tile_are_counted() {
    // 30 tiles each with a quantization of 60000 steps for its first component: 240 KB each when parsed.
    let mut tiles = Vec::new();
    for t in 0..30 {
        let mut qcc = vec![0u8, 0];
        qcc.extend(std::iter::repeat_n(10u8 << 3, 60_000));
        let mut extra = Vec::new();
        marker(&mut extra, 0xFF5D, &qcc);
        tiles.extend(tile_part_with(t, &extra, &[0x00; 4]));
    }
    let s = stream(&siz(480, 16, 16, 16, &[(8, 1, 1)]), 1, 1, (4, 4), 0, None, &tiles);
    let (r, _, _) = go(&s, (0, 0));
    let d = r.expect("picture");
    assert!(MAX_MEMORY - d.memory_low >= 30 * 240_000, "{} bytes", MAX_MEMORY - d.memory_low);
}

fn decode_with(data: &[u8], group_bytes: Option<u64>, o: &Options) -> Decoded {
    GROUP_BYTES_TEST.with(|c| c.set(group_bytes));
    let r = decode(data, o, &Work::new());
    GROUP_BYTES_TEST.with(|c| c.set(None));
    r.expect("picture")
}

#[test]
fn a_big_tile_is_decoded_one_component_at_a_time() {
    // The same pictures, with the limit for "big" at nothing: what comes out is what comes out of the small way
    // (exactly when the transform is the reversible one; to a step of 1/16 or less of the sample when it is not),
    // and the most memory held at a moment is less.
    // (The last two are big enough for the threads that share the work of the wavelet transform and the writing.)
    let cases = [(true, 8u8, 3usize, 150u32, 130u32), (true, 12, 3, 150, 130), (false, 8, 3, 150, 130), (false, 12, 3, 150, 130), (true, 8, 4, 150, 130), (false, 8, 5, 150, 130), (true, 8, 3, 600, 450), (false, 8, 3, 600, 450)];
    for (reversible, depth, comps, w, h) in cases {
        let mut cfg = Cfg::new(w, h, comps);
        cfg.levels = 3;
        cfg.cb = (4, 4);
        cfg.mct = true;
        cfg.reversible = reversible;
        for c in &mut cfg.comps {
            c.depth = depth;
        }
        let es = encode(&cfg).codestream;
        let o = Options { threads: Some(4), ..opts((0, 0)) };
        let small = decode_with(&es, None, &o);
        let big = decode_with(&es, Some(0), &o);
        assert_eq!((big.w, big.h, big.ncomp), (small.w, small.h, small.ncomp));
        let differ = small.data.iter().zip(&big.data).filter(|(a, b)| a != b).count();
        let most = small.data.iter().zip(&big.data).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        eprintln!("reversible {reversible} depth {depth} comps {comps} {w}x{h}: {differ} of {} differ, by {most} at most; held {} against {}", small.data.len(), MAX_MEMORY - big.memory_low, MAX_MEMORY - small.memory_low);
        if reversible {
            assert_eq!(differ, 0);
        } else {
            assert!(most <= 1 && differ * 25 < small.data.len(), "{differ} differ by {most}");
        }
        // (Lossy samples of 12 bits do not go in two bytes: the same memory is held.)
        if !reversible && depth > 8 {
            assert!(big.memory_low >= small.memory_low);
        } else {
            assert!(big.memory_low > small.memory_low, "{} against {}", MAX_MEMORY - big.memory_low, MAX_MEMORY - small.memory_low);
        }
        assert!(small.warning.is_none() && big.warning.is_none(), "{:?} {:?}", small.warning, big.warning);
    }
}

#[test]
fn the_dictionarys_colour_space_comes_before_the_files_sycc() {
    let mut cfg = Cfg::new(30, 20, 3);
    cfg.levels = 2;
    cfg.reversible = true;
    let es = encode(&cfg).codestream;
    let plain = jp2(&cfg, &es, 16, &[]);
    let sycc = jp2(&cfg, &es, 18, &[]);
    let with = |data: &[u8], components: Option<usize>| decode(data, &Options { components, ..opts((0, 0)) }, &Work::new()).expect("picture").data;
    // The file says sYCC and the dictionary says nothing: converted.
    assert_ne!(with(&sycc, None), with(&plain, None));
    // The dictionary has a colour space of three channels: the samples are in it, whatever the file says.
    assert_eq!(with(&sycc, Some(3)), with(&plain, Some(3)));
}
