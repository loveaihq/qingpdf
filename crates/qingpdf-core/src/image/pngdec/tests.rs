//! The PNG reader against the `png` crate (a development dependency only) and against files made to break it.

use std::io::Cursor;
use std::path::PathBuf;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::super::{MAX_INTERLACED_BYTES, MAX_PNG_PIXELS, PNG_WORKING_BYTES, PngImage, RowSink, bad_png, decode_png, decode_png_limited, png_too_large, probe};
use crate::error::{Error, Result};
use crate::render::export::crc32_update;

/// The reader of before the own one: the `png` crate, row by row. Everything that is decoded is compared with what it gives.
fn old_decode(data: &[u8], max_pixels: u64) -> Result<PngImage> {
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits { bytes: PNG_WORKING_BYTES });
    let mut reader = decoder.read_info().map_err(bad_png)?;
    let (width, height) = reader.info().size();
    if u64::from(width) * u64::from(height) > max_pixels.min(MAX_PNG_PIXELS) {
        return Err(png_too_large());
    }
    let (color_type, bit_depth) = reader.output_color_type();
    assert_eq!(bit_depth, png::BitDepth::Eight);
    let channels = match color_type {
        png::ColorType::Grayscale => 1usize,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(bad_png("palette was not expanded")),
    };
    let gray = matches!(color_type, png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha);
    let mut sink = RowSink::new(channels, width as usize);
    let mut rows = 0u64;
    if reader.info().interlaced {
        let line = reader.output_line_size(width).ok_or_else(png_too_large)?;
        let size = reader.output_buffer_size().filter(|&n| n <= MAX_INTERLACED_BYTES).ok_or_else(png_too_large)?;
        let mut buf = vec![0u8; size];
        reader.next_frame(&mut buf).map_err(bad_png)?;
        for row in buf.chunks_exact(line.max(1)) {
            sink.push_row(row)?;
            rows += 1;
        }
    } else {
        while let Some(row) = reader.next_row().map_err(bad_png)? {
            sink.push_row(row.data())?;
            rows += 1;
        }
    }
    if rows != u64::from(height) {
        return Err(bad_png("the pixel data is shorter than the header says"));
    }
    let (pixels_flate, alpha_flate) = sink.finish()?;
    let dpi = reader.info().pixel_dims.and_then(|p| match p.unit {
        png::Unit::Meter if p.xppu > 0 && p.yppu > 0 => Some((f64::from(p.xppu) * 0.0254, f64::from(p.yppu) * 0.0254)),
        _ => None,
    });
    Ok(PngImage { width, height, gray, pixels_flate, alpha_flate, dpi })
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("png");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect();
    paths.sort();
    paths.into_iter().map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap())).collect()
}

#[test]
fn every_file_made_by_the_script_is_read_as_the_png_crate_reads_it() {
    let files = fixtures();
    assert!(files.len() >= 200, "the files of tests/tools/make_png_fixtures.py are not all here: {}", files.len());
    let mut with_alpha = 0;
    for (name, data) in &files {
        let new = decode_png(data).unwrap_or_else(|e| panic!("{name}: {e}"));
        let old = old_decode(data, MAX_PNG_PIXELS).unwrap_or_else(|e| panic!("{name} (the png crate): {e}"));
        assert_eq!(new, old, "{name}");
        let (w, h, _) = probe(data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((w, h), (new.width, new.height), "{name}");
        with_alpha += usize::from(new.alpha_flate.is_some());
    }
    assert!(with_alpha > 20, "{with_alpha}");
}

/// Bytes that look like anything (a fixed seed).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (state >> 56) as u8
        })
        .collect()
}

#[test]
fn big_pictures_of_every_kind_and_every_filter_are_read_as_the_png_crate_reads_them() {
    use png::{BitDepth as D, ColorType as C};
    let kinds = [
        (C::Grayscale, D::One),
        (C::Grayscale, D::Two),
        (C::Grayscale, D::Four),
        (C::Grayscale, D::Eight),
        (C::Grayscale, D::Sixteen),
        (C::Rgb, D::Eight),
        (C::Rgb, D::Sixteen),
        (C::Indexed, D::One),
        (C::Indexed, D::Four),
        (C::Indexed, D::Eight),
        (C::GrayscaleAlpha, D::Eight),
        (C::GrayscaleAlpha, D::Sixteen),
        (C::Rgba, D::Eight),
        (C::Rgba, D::Sixteen),
    ];
    let filters = [png::Filter::NoFilter, png::Filter::Sub, png::Filter::Up, png::Filter::Avg, png::Filter::Paeth];
    for (n, &(color, depth)) in kinds.iter().enumerate() {
        let (width, height) = (301u32 + n as u32, 97u32 + n as u32);
        let bits = color.samples() * depth as usize;
        let row = (width as usize * bits).div_ceil(8);
        let raw = noise(row * height as usize, n as u64 + 1);
        for (f, &filter) in filters.iter().enumerate() {
            let mut data = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut data, width, height);
                enc.set_color(color);
                enc.set_depth(depth);
                enc.set_filter(filter);
                if color == C::Indexed {
                    enc.set_palette(noise(3 * 13, 5));
                    if f % 2 == 0 {
                        enc.set_trns(noise(9, 6));
                    }
                } else if matches!(color, C::Grayscale | C::Rgb) && f % 2 == 1 {
                    // (A colour that the noise is likely to have.)
                    enc.set_trns(raw.get(..2 * color.samples()).unwrap_or(&[]).to_vec());
                }
                let mut writer = enc.write_header().unwrap();
                writer.write_image_data(&raw).unwrap();
                writer.finish().unwrap();
            }
            let new = decode_png(&data).unwrap_or_else(|e| panic!("{color:?} {depth:?} {filter:?}: {e}"));
            let old = old_decode(&data, MAX_PNG_PIXELS).unwrap();
            assert_eq!(new, old, "{color:?} {depth:?} {filter:?}");
        }
    }
}

// --- files made by hand -------------------------------------------------------------------------------------------

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc32_update(crc32_update(0, kind), body).to_be_bytes());
    out
}

fn ihdr(width: u32, height: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
    let mut body = width.to_be_bytes().to_vec();
    body.extend_from_slice(&height.to_be_bytes());
    body.extend_from_slice(&[depth, color, 0, 0, interlace]);
    chunk(b"IHDR", &body)
}

fn file(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for c in chunks {
        out.extend_from_slice(c);
    }
    out
}

fn idat(raw: &[u8]) -> Vec<u8> {
    chunk(b"IDAT", &compress_to_vec_zlib(raw, 6))
}

/// A picture of `width` by `height` pixels, 8-bit RGB, every row with the filter `filter`.
fn rgb(width: u32, height: u32, filter: u8) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(filter);
        raw.extend(noise(width as usize * 3, u64::from(y)));
    }
    file(&[ihdr(width, height, 8, 2, 0), idat(&raw), chunk(b"IEND", &[])])
}

fn is_limit(r: Result<PngImage>) -> bool {
    matches!(r, Err(Error::Limit(_)))
}

#[test]
fn a_header_that_claims_a_huge_picture_is_refused_whatever_follows_it() {
    for (w, h, depth, color, interlace) in [(u32::MAX, u32::MAX, 8, 6, 0), (40_000, 40_000, 8, 0, 0), (u32::MAX, 1, 16, 6, 0), (1 << 28, 1, 8, 6, 0), (16_384, 16_384, 8, 6, 1), (1 << 27, 2, 1, 0, 1)] {
        let data = file(&[ihdr(w, h, depth, color, interlace), idat(&[0; 100]), chunk(b"IEND", &[])]);
        assert!(is_limit(decode_png(&data)), "{w} x {h}");
        assert!(old_decode(&data, MAX_PNG_PIXELS).is_err());
    }
    // A caller's own limit.
    let data = rgb(30, 20, 0);
    assert!(decode_png(&data).is_ok());
    assert!(is_limit(decode_png_limited(&data, 599)));
    assert!(decode_png_limited(&data, 600).is_ok());
    // A picture as big as is allowed that has no pixels in it is refused when the first row is not there, and costs nothing.
    let wide = file(&[ihdr(1 << 14, 1 << 14, 8, 0, 0), idat(&[0; 10]), chunk(b"IEND", &[])]);
    assert!(decode_png(&wide).is_err());
}

#[test]
fn a_file_that_is_cut_short_is_an_error_and_never_a_panic() {
    let full = rgb(41, 23, 4);
    let image_ends = full.len() - 12;
    for cut in 0..full.len() {
        let result = decode_png(&full[..cut]);
        if cut < image_ends {
            assert!(result.is_err(), "cut at {cut}");
            assert!(old_decode(&full[..cut], MAX_PNG_PIXELS).is_err(), "cut at {cut}");
        }
        let _ = probe(&full[..cut]);
    }
    // (Without the end chunk the picture is whole, and is read.)
    assert_eq!(decode_png(&full[..image_ends]).unwrap(), decode_png(&full).unwrap());
}

#[test]
fn the_rows_of_a_picture_that_are_not_all_there_are_an_error() {
    let mut raw = vec![0u8];
    raw.extend(noise(30, 1));
    for rows in [1, 2] {
        let data = file(&[ihdr(10, 3, 8, 2, 0), idat(&raw.repeat(rows)), chunk(b"IEND", &[])]);
        assert!(matches!(decode_png(&data), Err(Error::Invalid(_))), "{rows} rows");
        assert!(old_decode(&data, MAX_PNG_PIXELS).is_err());
    }
}

#[test]
fn a_row_with_a_filter_that_is_not_one_of_the_five_is_an_error() {
    for filter in [5u8, 9, 255] {
        let data = rgb(7, 5, filter);
        assert!(matches!(decode_png(&data), Err(Error::Invalid(_))), "{filter}");
        assert!(old_decode(&data, MAX_PNG_PIXELS).is_err(), "{filter}");
    }
    for filter in 0u8..5 {
        assert_eq!(decode_png(&rgb(7, 5, filter)).unwrap(), old_decode(&rgb(7, 5, filter), MAX_PNG_PIXELS).unwrap());
    }
}

#[test]
fn a_palette_index_past_the_end_of_the_palette_is_opaque_black() {
    let raw = [0u8, 0, 1, 2, 200];
    let palette = chunk(b"PLTE", &[255, 0, 0, 0, 255, 0]);
    for trns in [None, Some(vec![10u8]), Some(vec![10, 20])] {
        let mut chunks = vec![ihdr(4, 1, 8, 3, 0), palette.clone()];
        chunks.extend(trns.map(|t| chunk(b"tRNS", &t)));
        chunks.extend([idat(&raw), chunk(b"IEND", &[])]);
        let data = file(&chunks);
        let new = decode_png(&data).unwrap();
        assert_eq!(new, old_decode(&data, MAX_PNG_PIXELS).unwrap());
        let pixels = miniz_oxide::inflate::decompress_to_vec_zlib(&new.pixels_flate).unwrap();
        assert_eq!(pixels, [255, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0]);
    }
    // The same for the pixels of a picture of 2 bits a pixel with a palette of two colours.
    let data = file(&[ihdr(4, 1, 2, 3, 0), chunk(b"PLTE", &[1, 2, 3, 4, 5, 6]), idat(&[0, 0b0011_1001]), chunk(b"IEND", &[])]);
    assert_eq!(decode_png(&data).unwrap(), old_decode(&data, MAX_PNG_PIXELS).unwrap());
    // No palette at all.
    let data = file(&[ihdr(4, 1, 8, 3, 0), idat(&raw), chunk(b"IEND", &[])]);
    assert!(decode_png(&data).is_err());
    assert!(old_decode(&data, MAX_PNG_PIXELS).is_err());
}

#[test]
fn checksums_chunks_out_of_place_and_chunks_that_do_not_fit_are_dealt_with_as_the_png_crate_does() {
    let raw = [0u8, 10, 20, 30];
    let good = |extra: Vec<Vec<u8>>| {
        let mut chunks = vec![ihdr(1, 1, 8, 2, 0)];
        chunks.extend(extra);
        chunks.extend([idat(&raw), chunk(b"IEND", &[])]);
        file(&chunks)
    };
    let spoil = |mut c: Vec<u8>| {
        let n = c.len();
        c[n - 1] ^= 1;
        c
    };
    let same = |data: Vec<u8>| {
        let new = decode_png(&data);
        let old = old_decode(&data, MAX_PNG_PIXELS);
        assert_eq!(new.is_ok(), old.is_ok(), "{new:?} {old:?}");
        if let (Ok(new), Ok(old)) = (new, old) {
            assert_eq!(new, old);
        }
    };
    let phys = |unit: u8| {
        let mut body = 11811u32.to_be_bytes().to_vec();
        body.extend_from_slice(&5906u32.to_be_bytes());
        body.push(unit);
        body
    };
    same(good(vec![]));
    // A wrong checksum: an error for a chunk that has to be read, a chunk that is left out for one that does not.
    same(good(vec![spoil(chunk(b"pHYs", &phys(1)))]));
    same(good(vec![spoil(chunk(b"tRNS", &[0, 10, 0, 20, 0, 30]))]));
    same(good(vec![spoil(chunk(b"tEXt", b"Comment\0hello"))]));
    same(good(vec![spoil(chunk(b"PLTE", &[1, 2, 3]))]));
    same(file(&[ihdr(1, 1, 8, 2, 0), spoil(idat(&raw)), chunk(b"IEND", &[])]));
    same(file(&[spoil(ihdr(1, 1, 8, 2, 0)), idat(&raw), chunk(b"IEND", &[])]));
    // Chunks that are there twice, in a form that does not fit, or for a colour type that has none.
    same(good(vec![chunk(b"pHYs", &phys(1)), chunk(b"pHYs", &phys(0))]));
    same(good(vec![chunk(b"pHYs", &phys(7)), chunk(b"pHYs", &phys(1))]));
    same(good(vec![chunk(b"pHYs", &phys(1)[..8])]));
    same(good(vec![chunk(b"tRNS", &[0, 10, 0, 20, 0, 30]), chunk(b"tRNS", &[0, 1, 0, 1, 0, 1])]));
    same(good(vec![chunk(b"tRNS", &[0, 10, 0, 20])]));
    same(good(vec![chunk(b"tRNS", &[0, 10, 0, 20, 0, 30, 0, 0, 0])]));
    same(good(vec![chunk(b"tRNS", &[])]));
    same(good(vec![chunk(b"PLTE", &[1, 2, 3]), chunk(b"PLTE", &[1, 2, 3])]));
    same(good(vec![chunk(b"PLTE", &[1, 2, 3, 4])]));
    same(good(vec![chunk(b"PLTE", &[])]));
    same(good(vec![chunk(b"sRGB", &[0]), chunk(b"gAMA", &[0, 0, 0, 1]), chunk(b"zzZz", b"private")]));
    same(good(vec![chunk(b"ZZZZ", b"a critical chunk of no known kind")]));
    same(good(vec![chunk(b"IEND", &[])]));
    same(file(&[chunk(b"IEND", &[])]));
    same(file(&[ihdr(1, 1, 8, 2, 0), chunk(b"IEND", &[])]));
    same(file(&[idat(&raw), ihdr(1, 1, 8, 2, 0), chunk(b"IEND", &[])]));
    same(file(&[ihdr(1, 1, 8, 2, 0), ihdr(1, 1, 8, 2, 0), idat(&raw), chunk(b"IEND", &[])]));
    // The header.
    for (w, h, depth, color, interlace) in [(0, 1, 8, 2, 0), (1, 0, 8, 2, 0), (1, 1, 3, 2, 0), (1, 1, 1, 2, 0), (1, 1, 16, 3, 0), (1, 1, 8, 5, 0), (1, 1, 8, 7, 0), (1, 1, 8, 2, 2), (1, 1, 4, 6, 0), (1, 1, 2, 4, 1)] {
        same(file(&[ihdr(w, h, depth, color, interlace), idat(&[0, 1, 2, 3, 4, 5, 6, 7]), chunk(b"IEND", &[])]));
    }
    // A method of compression or of filtering of the header that is not 0.
    for at in [10, 11] {
        let mut head = ihdr(1, 1, 8, 2, 0);
        head[8 + at] = 1;
        let n = head.len();
        let crc = crc32_update(0, &head[4..n - 4]);
        head[n - 4..].copy_from_slice(&crc.to_be_bytes());
        same(file(&[head, idat(&raw), chunk(b"IEND", &[])]));
    }
}

#[test]
fn the_image_data_may_be_cut_into_any_pieces_and_what_comes_after_it_is_not_looked_at() {
    let raw = {
        let mut raw = Vec::new();
        for y in 0..40u64 {
            raw.push((y % 5) as u8);
            raw.extend(noise(60, y));
        }
        raw
    };
    let zlib = compress_to_vec_zlib(&raw, 6);
    let whole = file(&[ihdr(20, 40, 8, 2, 0), chunk(b"IDAT", &zlib), chunk(b"IEND", &[])]);
    let expected = decode_png(&whole).unwrap();
    assert_eq!(expected, old_decode(&whole, MAX_PNG_PIXELS).unwrap());
    for piece in [1usize, 2, 3, 7, 50, 1000] {
        let mut chunks = vec![ihdr(20, 40, 8, 2, 0)];
        for (n, part) in zlib.chunks(piece).enumerate() {
            chunks.push(chunk(b"IDAT", part));
            if n == 3 {
                // (An empty one.)
                chunks.push(chunk(b"IDAT", &[]));
            }
        }
        chunks.push(chunk(b"IEND", &[]));
        assert_eq!(decode_png(&file(&chunks)).unwrap(), expected, "{piece}");
    }
    // After the data: nothing, junk, a second run of image data, a chunk that is cut off.
    for tail in [vec![], b"junk that is no chunk".to_vec(), idat(&[0, 0, 0, 0]), chunk(b"IEND", &[])[..9].to_vec()] {
        let mut data = file(&[ihdr(20, 40, 8, 2, 0), chunk(b"IDAT", &zlib), chunk(b"tEXt", b"a\0b")]);
        data.extend_from_slice(&tail);
        assert_eq!(decode_png(&data).unwrap(), expected);
    }
    // More compressed data than the picture has rows: the rest is not decompressed (it is a lot).
    let mut bomb = raw.clone();
    bomb.resize(raw.len() + 50_000_000, 0);
    let data = file(&[ihdr(20, 40, 8, 2, 0), idat(&bomb), chunk(b"IEND", &[])]);
    assert_eq!(decode_png(&data).unwrap(), expected);
}

#[test]
fn data_that_is_not_a_zlib_stream_is_an_error() {
    let junk = file(&[ihdr(4, 4, 8, 0, 0), chunk(b"IDAT", &noise(200, 9)), chunk(b"IEND", &[])]);
    assert!(matches!(decode_png(&junk), Err(Error::Invalid(_))));
    assert!(old_decode(&junk, MAX_PNG_PIXELS).is_err());
    let empty = file(&[ihdr(4, 4, 8, 0, 0), chunk(b"IDAT", &[]), chunk(b"IEND", &[])]);
    assert!(decode_png(&empty).is_err());
    let mut raw = vec![0u8];
    raw.extend([1, 2, 3, 4]);
    let mut zlib = compress_to_vec_zlib(&raw.repeat(4), 6);
    // Not the checksum at the end, but the stream itself is damaged: the checksum is not looked at, the rest is.
    let n = zlib.len();
    zlib[n - 1] ^= 0xFF;
    let data = file(&[ihdr(4, 4, 8, 0, 0), chunk(b"IDAT", &zlib), chunk(b"IEND", &[])]);
    assert_eq!(decode_png(&data).unwrap(), old_decode(&data, MAX_PNG_PIXELS).unwrap());
    assert!(decode_png(b"\x89PNG\r\n\x1a").is_err());
    assert!(decode_png(b"").is_err());
}

#[test]
fn no_change_of_a_byte_makes_the_reader_panic() {
    let mut files = vec![rgb(9, 6, 4)];
    // An interlaced palette picture with a transparency chunk, and an interlaced 16-bit grey one with a transparent colour.
    let raw = noise(200, 3);
    files.push(file(&[ihdr(8, 8, 4, 3, 1), chunk(b"PLTE", &noise(48, 1)), chunk(b"tRNS", &[0, 128, 255]), idat(&raw), chunk(b"IEND", &[])]));
    files.push(file(&[ihdr(8, 8, 16, 0, 1), chunk(b"tRNS", &[1, 2]), idat(&raw), chunk(b"IEND", &[])]));
    for data in files {
        for at in 0..data.len() {
            for xor in [0x01u8, 0x80, 0xFF] {
                let mut copy = data.clone();
                copy[at] ^= xor;
                let _ = decode_png(&copy);
                let _ = probe(&copy);
            }
        }
    }
}
