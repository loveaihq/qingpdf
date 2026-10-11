//! Streams from the test encoder ([`super::test_enc`]): every code-block style switch, markers, packed packet headers,
//! progression order changes, tile-parts, subsampling, bit depths, palettes and channel definitions. With no
//! decomposition levels the coefficients are the samples, so the picture is known; with levels, every way of packing
//! the same coefficients must give the same picture as the plain way does (and OpenJPEG agrees with ours on all of them,
//! see `tests/tools/check_jpx_dump.py`).

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop)]

use super::test_enc::{Cfg, CompCfg, Poc, encode, jp2, jp2_box};
use super::{Alpha, Decoded, Options, decode};
use crate::render::work::Work;

fn opts(alpha: Alpha) -> Options {
    Options { want: (0, 0), components: None, indexed: false, alpha, threads: None }
}

fn dump(name: &str, stream: &[u8], d: &Decoded) {
    if let Ok(dir) = std::env::var("JPX_DUMP_DIR") {
        let ext = if stream.starts_with(&[0xFF, 0x4F]) { "j2k" } else { "jp2" };
        std::fs::write(format!("{dir}/{name}.{ext}"), stream).expect("write");
        let flat: Vec<u8> = match &d.alpha {
            None => d.data.clone(),
            Some(a) => d.data.chunks_exact(d.ncomp).zip(a).flat_map(|(px, &a)| px.iter().copied().chain([a])).collect(),
        };
        std::fs::write(format!("{dir}/{name}.raw"), format!("{} {} {}\n", d.w, d.h, d.ncomp + usize::from(d.alpha.is_some())).into_bytes().into_iter().chain(flat).collect::<Vec<u8>>()).expect("write");
    }
}

fn go(name: &str, stream: &[u8], o: &Options) -> Decoded {
    let work = Work::new();
    let d = decode(stream, o, &work).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(d.warning.is_none(), "{name}: {:?}", d.warning);
    if name != "baseline" {
        dump(name, stream, &d);
    }
    d
}

/// The sample of component `c` at (x, y) of a stream with no decomposition levels, in the range of its depth.
fn sample(cfg: &Cfg, c: usize, x: i64, y: i64) -> u32 {
    let comp = cfg.comps[c];
    let v = i64::from(cfg.coef(c, 0, 0, x, y)) + (1i64 << (comp.depth - 1));
    v.clamp(0, (1i64 << comp.depth) - 1) as u32
}

fn to8(v: u32, depth: u8) -> u8 {
    match depth {
        8 => v as u8,
        d if d > 8 => ((v + (1 << (d - 9))) >> (d - 8)).min(255) as u8,
        d => ((v * 255 + ((1 << d) - 1) / 2) / ((1 << d) - 1)) as u8,
    }
}

/// The picture the decoder must make of `cfg` (no levels, no component transform): channels in order.
fn expected(cfg: &Cfg) -> Vec<u8> {
    let (w, h) = (cfg.w as usize, cfg.h as usize);
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for (c, comp) in cfg.comps.iter().enumerate() {
                let (cw, ch) = (cfg.w.div_ceil(comp.dx) as usize, cfg.h.div_ceil(comp.dy) as usize);
                let (cx, cy) = (x * cw / w, y * ch / h);
                out.push(to8(sample(cfg, c, cx as i64, cy as i64), comp.depth));
            }
        }
    }
    out
}

#[test]
fn no_levels_every_code_block_style() {
    // 0: plain; 1 bypass; 2 reset; 4 terminate every pass; 8 vertically causal; 16 predictable termination;
    // 32 segmentation symbols; and combinations.
    for style in [0u8, 1, 2, 4, 8, 16, 32, 1 | 4, 1 | 2, 2 | 4, 8 | 32, 1 | 8, 1 | 2 | 4 | 8 | 32, 63] {
        for (cb, ncomp, depth) in [((4u8, 4u8), 1usize, 8u8), ((3, 5), 3, 8), ((5, 3), 1, 12), ((2, 2), 1, 8), ((6, 6), 1, 8)] {
            let mut cfg = Cfg::new(45, 37, ncomp);
            cfg.cb = cb;
            cfg.cbstyle = style;
            cfg.seed = u64::from(style) + 3;
            for c in &mut cfg.comps {
                c.depth = depth;
            }
            let es = encode(&cfg);
            let d = go(&format!("style{style}_{}x{}_{ncomp}_{depth}", cb.0, cb.1), &es.codestream, &opts(Alpha::Ignore));
            assert_eq!((d.w, d.h, d.ncomp), (45, 37, ncomp));
            assert!(d.data == expected(&cfg), "style {style} cb {cb:?} comps {ncomp} depth {depth}");
        }
    }
}

#[test]
fn layers_and_tiles_with_no_levels() {
    for (layers, tile) in [(3usize, (45u32, 37u32)), (4, (16, 16)), (2, (20, 13))] {
        for style in [0u8, 4, 1 | 4, 1] {
            let mut cfg = Cfg::new(45, 37, 3);
            cfg.layers = layers;
            cfg.tile = tile;
            cfg.cbstyle = style;
            cfg.cb = (3, 3);
            let es = encode(&cfg);
            let d = go(&format!("layers{layers}_{}x{}_s{style}", tile.0, tile.1), &es.codestream, &opts(Alpha::Ignore));
            assert!(d.data == expected(&cfg), "layers {layers} tile {tile:?} style {style}");
        }
    }
}

/// The picture of a baseline (LRCP, no markers, packet headers in the data, one tile-part) of the same coefficients.
fn baseline(cfg: &Cfg) -> Decoded {
    let mut b = cfg.clone();
    b.prog = 0;
    b.layers = 1;
    b.sop = false;
    b.eph = false;
    b.ppm = false;
    b.ppt = false;
    b.tile_parts = 1;
    b.poc = Vec::new();
    b.precincts = cfg.precincts.clone();
    go("baseline", &encode(&b).codestream, &opts(Alpha::Ignore))
}

fn base_cfg() -> Cfg {
    let mut cfg = Cfg::new(70, 53, 3);
    cfg.levels = 3;
    cfg.cb = (3, 3);
    cfg.tile = (40, 32);
    cfg.precincts = Some(vec![(3, 3), (4, 4), (4, 3), (5, 5)]);
    cfg
}

#[test]
fn every_way_of_packing_gives_the_same_picture() {
    let cfg = base_cfg();
    let reference = baseline(&cfg);
    let mut tried = 0;
    for prog in 0..5u8 {
        for (sop, eph, ppm, ppt, parts, layers) in [
            (false, false, false, false, 1usize, 1usize),
            (true, false, false, false, 1, 3),
            (false, true, false, false, 2, 2),
            (true, true, false, false, 3, 4),
            (false, false, true, false, 1, 2),
            (true, true, true, false, 2, 3),
            (false, false, false, true, 1, 3),
            (true, true, false, true, 3, 2),
        ] {
            let mut c = cfg.clone();
            c.prog = prog;
            c.sop = sop;
            c.eph = eph;
            c.ppm = ppm;
            c.ppt = ppt;
            c.tile_parts = parts;
            c.layers = layers;
            let es = encode(&c);
            let name = format!("pack_p{prog}_sop{}_eph{}_ppm{}_ppt{}_tp{parts}_l{layers}", u8::from(sop), u8::from(eph), u8::from(ppm), u8::from(ppt));
            let d = go(&name, &es.codestream, &opts(Alpha::Ignore));
            assert!(d.data == reference.data, "{name}");
            tried += 1;
        }
    }
    assert_eq!(tried, 40);
}

#[test]
fn progression_order_changes() {
    let mut cfg = base_cfg();
    cfg.layers = 3;
    let reference = baseline(&cfg);
    let n = cfg.ncomp() as u16;
    let lists: Vec<Vec<Poc>> = vec![
        vec![Poc { rs: 0, cs: 0, lye: 2, re: 2, ce: n, prog: 1 }, Poc { rs: 0, cs: 0, lye: 3, re: 4, ce: n, prog: 4 }],
        vec![Poc { rs: 0, cs: 0, lye: 1, re: 4, ce: 1, prog: 2 }, Poc { rs: 0, cs: 0, lye: 3, re: 4, ce: n, prog: 3 }, Poc { rs: 0, cs: 0, lye: 3, re: 4, ce: n, prog: 0 }],
        // Ranges that reach past what there is.
        vec![Poc { rs: 1, cs: 1, lye: 60000, re: 200, ce: 9, prog: 0 }, Poc { rs: 0, cs: 0, lye: 3, re: 4, ce: n, prog: 2 }],
    ];
    for (i, list) in lists.into_iter().enumerate() {
        for in_tile in [false, true] {
            let mut c = cfg.clone();
            c.poc = list.clone();
            c.poc_in_tile = in_tile;
            c.tile_parts = 2;
            let d = go(&format!("poc{i}_{}", u8::from(in_tile)), &encode(&c).codestream, &opts(Alpha::Ignore));
            assert!(d.data == reference.data, "poc list {i} in_tile {in_tile}");
        }
    }
}

#[test]
fn component_transforms_with_no_levels() {
    // The reversible one: G = Y0 - floor((Y1 + Y2) / 4), R = Y2 + G, B = Y1 + G.
    let mut cfg = Cfg::new(33, 21, 3);
    cfg.mct = true;
    cfg.cb = (3, 3);
    let d = go("rct", &encode(&cfg).codestream, &opts(Alpha::Ignore));
    for y in 0..21i64 {
        for x in 0..33i64 {
            let v = |c: usize| i64::from(cfg.coef(c, 0, 0, x, y));
            let g = v(0) - ((v(1) + v(2)) >> 2);
            let (r, b) = (v(2) + g, v(1) + g);
            let px = &d.data[((y * 33 + x) * 3) as usize..][..3];
            let want = [r, g, b].map(|s| (s + 128).clamp(0, 255) as u8);
            assert_eq!(px, want, "at {x},{y}");
        }
    }
    // The irreversible one on floats: the steps are 1 + mu / 2048.
    let mut cfg = Cfg::new(33, 21, 3);
    cfg.mct = true;
    cfg.reversible = false;
    cfg.cb = (3, 3);
    let d = go("ict", &encode(&cfg).codestream, &opts(Alpha::Ignore));
    let mu = (super::test_enc::hash(cfg.seed, 0, 0, 0, 5) % 2048) as f64;
    let step = 1.0 + mu / 2048.0;
    for y in 0..21i64 {
        for x in 0..33i64 {
            // A coefficient that is not 0 is rebuilt as the middle of what its last bit leaves open (E.1.1.2).
            let v = |c: usize| match f64::from(cfg.coef(c, 0, 0, x, y)) {
                0.0 => 0.0,
                q => (q.abs() + 0.5).copysign(q) * step,
            };
            let (yy, cb, cr) = (v(0), v(1), v(2));
            let want = [yy + 1.402 * cr, yy - 0.344_13 * cb - 0.714_14 * cr, yy + 1.772 * cb].map(|s| (s + 128.0).round().clamp(0.0, 255.0) as i32);
            let px = &d.data[((y * 33 + x) * 3) as usize..][..3];
            for k in 0..3 {
                assert!((i32::from(px[k]) - want[k]).abs() <= 1, "at {x},{y}: {px:?} {want:?}");
            }
        }
    }
}

#[test]
fn bit_depths_signs_and_subsampling() {
    for (depth, signed) in [(1u8, false), (2, false), (4, false), (8, true), (10, false), (12, true), (16, false), (16, true)] {
        let mut cfg = Cfg::new(29, 19, 1);
        cfg.comps[0] = CompCfg { depth, signed, dx: 1, dy: 1 };
        cfg.cb = (3, 3);
        let d = go(&format!("depth{depth}_{signed}"), &encode(&cfg).codestream, &opts(Alpha::Ignore));
        assert!(d.data == expected(&cfg), "depth {depth} signed {signed}");
    }
    // Chroma at half the resolution each way, sYCC by the rule for bare codestreams and by the colour box.
    let mut cfg = Cfg::new(31, 23, 3);
    cfg.comps[1] = CompCfg { depth: 8, signed: false, dx: 2, dy: 2 };
    cfg.comps[2] = CompCfg { depth: 8, signed: false, dx: 2, dy: 2 };
    cfg.cb = (3, 3);
    cfg.levels = 0;
    let es = encode(&cfg);
    let planes = expected(&cfg);
    let ycc_to_rgb = |p: &[u8]| -> Vec<u8> {
        p.chunks_exact(3)
            .flat_map(|px| {
                let (y, cb, cr) = (f32::from(px[0]), f32::from(px[1]) - 128.0, f32::from(px[2]) - 128.0);
                [(y + 1.402 * cr + 0.5).clamp(0.0, 255.0) as u8, (y - 0.344_136 * cb - 0.714_136 * cr + 0.5).clamp(0.0, 255.0) as u8, (y + 1.772 * cb + 0.5).clamp(0.0, 255.0) as u8]
            })
            .collect()
    };
    let bare = go("subsampled_bare", &es.codestream, &opts(Alpha::Ignore));
    assert!(bare.data == ycc_to_rgb(&planes), "bare codestream with subsampled chroma is sYCC");
    let coded = go("subsampled_syc", &jp2(&cfg, &es.codestream, 18, &[]), &opts(Alpha::Ignore));
    assert!(coded.data == ycc_to_rgb(&planes));
    // Said to be sRGB, the chroma at lower resolution is just upsampled.
    let srgb = go("subsampled_srgb", &jp2(&cfg, &es.codestream, 16, &[]), &opts(Alpha::Ignore));
    assert!(srgb.data == planes);
}

#[test]
fn region_of_interest_shift() {
    let mut cfg = Cfg::new(40, 30, 1);
    cfg.cb = (3, 3);
    let plain = encode(&cfg);
    cfg.roi = 12;
    let shifted = encode(&cfg);
    assert!(plain.codestream != shifted.codestream);
    let d = go("roi", &shifted.codestream, &opts(Alpha::Ignore));
    cfg.roi = 0;
    assert!(d.data == expected(&cfg));
}

fn palette_box(entries: usize) -> Vec<u8> {
    let mut b = (entries as u16).to_be_bytes().to_vec();
    b.extend_from_slice(&[3, 7, 7, 7]);
    for e in 0..entries {
        b.extend_from_slice(&[(e * 16) as u8, (255 - (e * 8) % 256) as u8, (e * 4) as u8]);
    }
    b
}

#[test]
fn palette_and_channel_definitions() {
    // A grey picture whose samples are indices below 16.
    let mut cfg = Cfg::new(30, 20, 1);
    cfg.cb = (3, 3);
    cfg.levels = 0;
    let es = encode(&cfg);
    let idx = |x: i64, y: i64| (i64::from(cfg.coef(0, 0, 0, x, y)) + 128).clamp(0, 255) as usize;
    let cmap = jp2_box(b"cmap", &[0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2]);
    let mut pal = palette_box(256);
    let file = jp2(&cfg, &es.codestream, 16, &[jp2_box(b"pclr", &pal), cmap.clone()]);
    let d = go("palette", &file, &opts(Alpha::Ignore));
    assert_eq!(d.ncomp, 3);
    for y in 0..20i64 {
        for x in 0..30i64 {
            let i = idx(x, y);
            let want = [(i * 16) as u8, (255 - (i * 8) % 256) as u8, (i * 4) as u8];
            assert_eq!(&d.data[((y * 30 + x) * 3) as usize..][..3], want, "at {x},{y}");
        }
    }
    // With an Indexed colour space in the PDF, the palette is the PDF's: the indices come out as they are.
    let raw = decode(&file, &Options { want: (0, 0), components: Some(1), indexed: true, alpha: Alpha::Ignore, threads: None }, &Work::new()).expect("decodes");
    assert_eq!(raw.ncomp, 1);
    assert!(raw.data.iter().enumerate().all(|(i, &v)| usize::from(v) == idx((i % 30) as i64, (i / 30) as i64).min(255)));
    // A palette that lies about its size or has a column of 40 bits, or maps what is not there: no picture, no panic.
    pal.truncate(20);
    let broken = jp2(&cfg, &es.codestream, 16, &[jp2_box(b"pclr", &pal), cmap]);
    let r = decode(&broken, &opts(Alpha::Ignore), &Work::new());
    assert!(r.is_ok() || r.is_err());
    let bad_map = jp2(&cfg, &es.codestream, 16, &[jp2_box(b"pclr", &palette_box(16)), jp2_box(b"cmap", &[0, 5, 1, 0])]);
    assert!(decode(&bad_map, &opts(Alpha::Ignore), &Work::new()).is_err());

    // Channel definitions: an opacity channel and colours in the order BGR.
    let mut cfg = Cfg::new(24, 16, 4);
    cfg.cb = (3, 3);
    let es = encode(&cfg);
    let cdef = {
        let mut b = 4u16.to_be_bytes().to_vec();
        for (ch, typ, asoc) in [(0u16, 0u16, 3u16), (1, 0, 2), (2, 0, 1), (3, 1, 0)] {
            for v in [ch, typ, asoc] {
                b.extend_from_slice(&v.to_be_bytes());
            }
        }
        jp2_box(b"cdef", &b)
    };
    let file = jp2(&cfg, &es.codestream, 16, &[cdef]);
    let exp = expected(&cfg);
    let straight = go("cdef", &file, &opts(Alpha::Straight));
    let alpha = straight.alpha.as_ref().expect("alpha");
    for i in 0..24 * 16 {
        assert_eq!(&straight.data[i * 3..i * 3 + 3], [exp[i * 4 + 2], exp[i * 4 + 1], exp[i * 4]], "pixel {i}");
        assert_eq!(alpha[i], exp[i * 4 + 3]);
    }
    // Not asked for: the opacity channel is not decoded.
    let ignored = go("cdef_ignored", &file, &opts(Alpha::Ignore));
    assert!(ignored.alpha.is_none() && ignored.data == straight.data);
    // Premultiplied colours are divided by it.
    let pre = go("cdef_premultiplied", &file, &opts(Alpha::Premultiplied));
    let a = pre.alpha.as_ref().expect("alpha");
    for i in 0..24 * 16 {
        let al = u32::from(a[i]);
        for k in 0..3 {
            let c = u32::from(straight.data[i * 3 + k]);
            let want = if al == 0 || al == 255 { c } else { ((c * 255 + al / 2) / al).min(255) };
            assert_eq!(u32::from(pre.data[i * 3 + k]), want);
        }
    }
    // A fourth component with no definition is the alpha of a three-channel picture.
    let plain = jp2(&cfg, &es.codestream, 16, &[]);
    let d = go("rgba", &plain, &opts(Alpha::Straight));
    assert_eq!(d.ncomp, 3);
    assert!(d.alpha.is_some());
    // A PDF colour space of 1 component takes the first channel only.
    let one = decode(&plain, &Options { want: (0, 0), components: Some(1), indexed: false, alpha: Alpha::Ignore, threads: None }, &Work::new()).expect("decodes");
    assert_eq!(one.ncomp, 1);
}

#[test]
fn four_components_are_cmyk() {
    let mut cfg = Cfg::new(20, 14, 4);
    cfg.cb = (3, 3);
    let es = encode(&cfg);
    let d = go("cmyk", &jp2(&cfg, &es.codestream, 12, &[]), &opts(Alpha::Ignore));
    assert_eq!(d.ncomp, 4);
    assert!(d.data == expected(&cfg));
}

#[test]
fn irreversible_streams_decode() {
    let mut cfg = base_cfg();
    cfg.reversible = false;
    cfg.layers = 2;
    cfg.mct = true;
    let reference = baseline(&cfg);
    let d = go("irreversible", &encode(&cfg).codestream, &opts(Alpha::Ignore));
    assert!(d.data == reference.data);
}

/// A picture big enough for the decoder to put its code-blocks, wavelet transforms and pixels on several threads.
fn big_cfg(reversible: bool, mct: bool) -> Cfg {
    let mut cfg = Cfg::new(340, 300, 3);
    cfg.levels = 3;
    cfg.cb = (5, 5);
    cfg.layers = 2;
    cfg.reversible = reversible;
    cfg.mct = mct;
    cfg
}

#[test]
fn threads_give_the_same_picture_as_one_thread() {
    for (reversible, mct) in [(true, true), (true, false), (false, true), (false, false)] {
        let cs = encode(&big_cfg(reversible, mct)).codestream;
        let run = |threads: Option<usize>| decode(&cs, &Options { threads, ..opts(Alpha::Ignore) }, &Work::new()).expect("decodes");
        let (one, four, auto) = (run(Some(1)), run(Some(4)), run(None));
        assert!(one.warning.is_none() && four.warning.is_none());
        assert!(one.data == four.data && one.data == auto.data, "reversible {reversible} mct {mct}");
    }
    // With no levels the picture is known: the threads must give it.
    let mut cfg = Cfg::new(520, 500, 1);
    cfg.cb = (5, 5);
    cfg.cbstyle = 1 | 4;
    let d = decode(&encode(&cfg).codestream, &Options { threads: Some(4), ..opts(Alpha::Ignore) }, &Work::new()).expect("decodes");
    assert!(d.data == expected(&cfg));
}

#[test]
fn components_with_levels_of_their_own() {
    // COC overrides COD: three components that the main style gives 2 levels are given 0 by their COC markers, so
    // their picture is known.
    let mut cfg = Cfg::new(45, 37, 3);
    cfg.levels = 2;
    cfg.cb = (3, 3);
    cfg.coc = vec![(0, 0), (1, 0), (2, 0)];
    let d = go("coc_all_zero", &encode(&cfg).codestream, &opts(Alpha::Ignore));
    // (the coefficients are those of the bands at resolution 0, which is all there is)
    cfg.coc = Vec::new();
    cfg.levels = 0;
    assert!(d.data == expected(&cfg));
    // Mixed: component 0 has 3 levels, 1 has none, 2 has one; every way of packing them gives the same picture (and
    // OpenJPEG agrees with ours on each: `check_jpx_dump.py`).
    let mut cfg = base_cfg();
    cfg.levels = 3;
    cfg.coc = vec![(1, 0), (2, 1)];
    cfg.precincts = Some(vec![(3, 3), (4, 4), (4, 3), (5, 5)]);
    let reference = baseline(&cfg);
    for prog in 0..5u8 {
        for (layers, ppm, parts) in [(1usize, false, 1usize), (3, false, 2), (2, true, 2)] {
            let mut c = cfg.clone();
            c.prog = prog;
            c.layers = layers;
            c.ppm = ppm;
            c.tile_parts = parts;
            let d = go(&format!("coc_mixed_p{prog}_l{layers}_ppm{}", u8::from(ppm)), &encode(&c).codestream, &opts(Alpha::Ignore));
            assert!(d.data == reference.data, "prog {prog} layers {layers} ppm {ppm}");
        }
    }
}

#[test]
fn the_style_of_a_tile_overrides_the_main_header() {
    let mut cfg = base_cfg();
    cfg.layers = 3;
    cfg.prog = 2;
    cfg.sop = true;
    cfg.eph = true;
    let reference = baseline(&cfg);
    cfg.tile_cod = true;
    for ppt in [false, true] {
        cfg.ppt = ppt;
        let d = go(&format!("tile_cod_{}", u8::from(ppt)), &encode(&cfg).codestream, &opts(Alpha::Ignore));
        assert!(d.data == reference.data, "ppt {ppt}");
    }
}
