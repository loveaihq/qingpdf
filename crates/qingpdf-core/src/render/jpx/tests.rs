//! Decoding of JPEG 2000 files made by another encoder (OpenJPEG, through `tests/tools/make_jpx_fixtures.py`) and
//! checked against what that library decodes them to.

use super::{Alpha, Decoded, Options, decode};
use crate::render::work::Work;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/jpx_fixtures/").to_string() + name).expect("fixture")
}

fn go(name: &str, want: (usize, usize), alpha: Alpha) -> Decoded {
    let work = Work::new();
    let opts = Options { want, components: None, indexed: false, alpha, threads: None };
    decode(&fixture(&format!("{name}.jp2")), &opts, &work).expect("decodes")
}

/// Interleaved channels, alpha last when there is one.
fn flat(d: &Decoded) -> Vec<u8> {
    match &d.alpha {
        None => d.data.clone(),
        Some(a) => d.data.chunks_exact(d.ncomp).zip(a).flat_map(|(px, &a)| px.iter().copied().chain([a])).collect(),
    }
}

/// (largest difference, mean difference)
fn diff(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len(), "sizes differ");
    let max = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let sum: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    (max, sum as f64 / a.len().max(1) as f64)
}

fn expected(name: &str) -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/jpx_fixtures/").to_string() + name).expect("expected")
}

/// Size of the picture at full resolution.
fn size(name: &str) -> (usize, usize) {
    let d = go(name, (0, 0), Alpha::Ignore);
    (d.w, d.h)
}

fn check(name: &str, raw: &str, reduce: u32, alpha: Alpha, max_allowed: u8) {
    let (w, h) = size(name);
    let want = (w.div_ceil(1 << reduce), h.div_ceil(1 << reduce));
    let d = go(name, want, alpha);
    assert_eq!((d.w, d.h), want, "{name} reduce {reduce}");
    let (max, mean) = diff(&flat(&d), &expected(raw));
    eprintln!("{name} r{reduce}: max {max} mean {mean:.4}");
    assert!(max <= max_allowed, "{name} reduce {reduce}: largest difference {max} (mean {mean})");
}

#[test]
fn lossless_files_come_out_exact() {
    for name in ["rgb_lossless", "gray_lossless", "rgb_tiles", "rgb_offset", "blocks_small", "gray16", "plt", "one_level", "tiny"] {
        check(name, &format!("{name}.raw"), 0, Alpha::Ignore, u8::from(name == "gray16"));
    }
    check("rgba_lossless", "rgba_lossless.raw", 0, Alpha::Straight, 0);
}

#[test]
fn every_progression_order_gives_the_same_picture() {
    for order in ["lrcp", "rlcp", "rpcl", "pcrl", "cprl"] {
        check(&format!("order_{order}"), "order.raw", 0, Alpha::Ignore, 0);
    }
}

#[test]
fn lossy_files_match_openjpeg_within_rounding() {
    for name in ["rgb_lossy", "gray_lossy", "rgb_tiles_lossy"] {
        check(name, &format!("{name}.raw"), 0, Alpha::Ignore, 1);
    }
}

#[test]
fn reduced_resolutions_match_openjpeg() {
    for (name, r) in [("rgb_lossless", 1), ("gray_lossless", 1), ("gray_lossless", 3), ("rgb_tiles", 1), ("rgb_lossy", 1), ("gray_lossy", 1), ("rgb_tiles_lossy", 1)] {
        check(name, &format!("{name}_r{r}.raw"), r, Alpha::Ignore, 1);
    }
}

/// A file named by `JPX_FILE`, decoded at `JPX_REDUCE` levels fewer and compared with the raw file `JPX_RAW` (a
/// debugging aid: `cargo test jpx::tests::compare_a_file -- --ignored --nocapture`).
#[test]
#[ignore]
fn compare_a_file() {
    let path = std::env::var("JPX_FILE").expect("JPX_FILE");
    let reduce: u32 = std::env::var("JPX_REDUCE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let raw = std::fs::read(std::env::var("JPX_RAW").expect("JPX_RAW")).expect("raw");
    let data = std::fs::read(path).expect("file");
    let work = Work::new();
    let full = decode(&data, &Options { want: (0, 0), components: None, indexed: false, alpha: Alpha::Ignore, threads: None }, &work).expect("decodes");
    let want = (full.w.div_ceil(1 << reduce), full.h.div_ceil(1 << reduce));
    let d = decode(&data, &Options { want, components: None, indexed: false, alpha: Alpha::Ignore, threads: None }, &Work::new()).expect("decodes");
    let (max, mean) = diff(&flat(&d), &raw);
    eprintln!("{}x{} reduce {reduce}: max {max} mean {mean:.4}", d.w, d.h);
    if let Ok(out) = std::env::var("JPX_OUT") {
        std::fs::write(out, flat(&d)).expect("write");
    }
}

/// The time and the work (meter units, about a nanosecond each) of decoding a file (a debugging aid and how the weights in
/// `work.rs` are checked): `JPX_FILE=... JPX_WANT=w,h JPX_THREADS=n cargo test --release jpx::tests::time_a_file --
/// --ignored --nocapture`.
#[test]
#[ignore]
fn time_a_file() {
    let path = std::env::var("JPX_FILE").expect("JPX_FILE");
    let want: Vec<usize> = std::env::var("JPX_WANT").unwrap_or_default().split(',').filter_map(|v| v.parse().ok()).collect();
    let data = std::fs::read(path).expect("file");
    let threads = std::env::var("JPX_THREADS").ok().and_then(|v| v.parse().ok());
    let o = Options { want: (want.first().copied().unwrap_or(0), want.get(1).copied().unwrap_or(0)), components: None, indexed: false, alpha: Alpha::Ignore, threads };
    for _ in 0..3 {
        let work = Work::new();
        let t = std::time::Instant::now();
        let d = decode(&data, &o, &work).expect("decodes");
        eprintln!("{}x{}: {:?}, {:.0} units", d.w, d.h, t.elapsed(), work.used());
    }
}
