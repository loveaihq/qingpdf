//! The JPEG encoder of `compress` writes files that other decoders must read as well as ours: this test writes them (and the pictures they
//! came from, as PNM files) to `tests/out/jpeg-encoder/` for `tests/tools/jpeg_check.py`, which opens them with Pillow and PyMuPDF and
//! measures how close they are. Here our own decoder reads each one and finds the size it was made at.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use qingpdf_core::render::image::jpeg_samples;
use qingpdf_core::squeeze::jpeg::{Raster, encode};

/// A picture: smooth colour, a little noise (a fixed generator), a hard edge and a thin line, so that all the coefficients are met.
fn picture(w: usize, h: usize, comps: usize) -> Vec<u8> {
    let mut seed = 4242u32;
    let mut out = Vec::with_capacity(w * h * comps);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            for c in 0..comps {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = f64::from(seed >> 24) / 255.0 * 8.0;
                let mut v = 120.0 + 80.0 * (fx * (3.0 + c as f64)).sin() * (fy * 2.0 + 0.3 * c as f64).cos() + 25.0 * fy + noise;
                if fx > 0.5 && fy < 0.5 {
                    v += 60.0;
                }
                if x == w / 3 || y == h / 3 {
                    v = 255.0 - v;
                }
                out.push(v.clamp(0.0, 255.0) as u8);
            }
        }
    }
    out
}

#[test]
fn the_encoders_files_are_written_for_other_decoders_and_ours_reads_them_at_the_size_they_were_made_at() {
    let dir = common::fresh_out_dir("jpeg-encoder");
    // (name, width, height, samples to a pixel, quality)
    let cases: [(&str, usize, usize, usize, u8); 17] = [
        ("1x1-rgb", 1, 1, 3, 75),
        ("1x1-grey", 1, 1, 1, 75),
        ("1x65500-rgb", 1, 65_500, 3, 75),
        ("65500x1-grey", 65_500, 1, 1, 75),
        ("7x9-rgb", 7, 9, 3, 75),
        ("8x8-grey", 8, 8, 1, 75),
        ("17x15-rgb", 17, 15, 3, 75),
        ("100x75-grey", 100, 75, 1, 75),
        ("1000x3-rgb", 1000, 3, 3, 75),
        ("3x1000-grey", 3, 1000, 1, 75),
        ("641x479-rgb-q1", 641, 479, 3, 1),
        ("641x479-rgb-q60", 641, 479, 3, 60),
        ("641x479-rgb-q75", 641, 479, 3, 75),
        ("641x479-rgb-q85", 641, 479, 3, 85),
        ("641x479-rgb-q100", 641, 479, 3, 100),
        ("641x479-grey-q75", 641, 479, 1, 75),
        ("1920x1080-rgb-q75", 1920, 1080, 3, 75),
    ];
    for (name, w, h, comps, quality) in cases {
        let data = picture(w, h, comps);
        let jpeg = encode(&Raster { w, h, comps, data: &data }, quality).unwrap();
        let (dw, dh, back) = jpeg_samples(&jpeg, comps, usize::MAX).unwrap();
        assert_eq!((dw, dh, back.len()), (w, h, data.len()), "{name}");
        std::fs::write(dir.join(format!("{name}.jpg")), &jpeg).unwrap();
        let mut pnm = format!("{}\n{w} {h}\n255\n", if comps == 1 { "P5" } else { "P6" }).into_bytes();
        pnm.extend_from_slice(&data);
        std::fs::write(dir.join(format!("{name}.pnm")), pnm).unwrap();
    }
}
