//! Redaction of a picture (2026-10-20; ISO 32000-1 8.9.5, 8.9.6): the pixels that lie under a mark are painted over in the picture itself, and the
//! picture is coded again. What was under the mark is not in the new picture; the old picture is not written to the new file unless something else
//! draws it.
//!
//! A picture is drawn into the unit square, which the current matrix puts on the page: the pixel at column `x`, row `y` of a picture of `w` by `h`
//! pixels is the part `[x/w, (x+1)/w] x [1-(y+1)/h, 1-y/h]` of the square. A mark is a rectangle of default user space; the pixels that its
//! rectangle touches (any part of the pixel) are the ones painted. The picture is read the way the renderer reads it (every filter and codec,
//! every colour space, a soft mask or a stencil mask folded into an opacity) and written back as plain samples: grey or RGB, 8 bits, with the
//! soft mask it needs; a stencil mask stays a stencil mask. Pictures of JPEG files come out as JPEG files, the rest Flate-coded with PNG "up"
//! prediction.

use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, Object, Stream};
use crate::render::export::decode_image;
use crate::render::work::{Work, cost};
use crate::squeeze::jpeg::{self, Raster};

/// Most pixels a picture that is redacted may have.
pub(super) const MAX_PIXELS: usize = 100 << 20;
/// The JPEG quality a picture that was a JPEG file is coded with again.
const QUALITY: u8 = 95;
/// How finely the work meter is charged for painting a pixel.
const PAINT_COST: f64 = 2.0;
/// A rectangle must reach this far (in pixels) into a pixel to take it: the edge of a mark that was cut from the edge of a pixel of the
/// picture does not take the pixel next to it.
const EDGE: f64 = 1.0e-3;

/// What came of a picture.
pub(super) enum Outcome {
    /// No pixel of the picture is under a mark.
    Clear,
    /// The picture as it is to be, and the soft mask it needs.
    Painted(Stream, Option<Stream>),
    /// The picture cannot be read or coded again: it is to be left out of the page altogether.
    Gone,
}

/// A mark in the pixels of a picture: the first and one past the last column and row of the pixels its box touches. (A picture that is turned by
/// other than a quarter has the box of the mark in its own pixels turned too; the pixels of the box that holds it are all painted, which is more
/// than the mark: never less.)
struct Region {
    x: (usize, usize),
    y: (usize, usize),
}

/// The marks `rects` (default user space) in the pixels of a `w` by `h` picture drawn under `ctm`. A mark the picture does not reach gives nothing.
fn regions(ctm: &[f64; 6], rects: &[[f64; 4]], w: usize, h: usize) -> Vec<Region> {
    let det = ctm[0] * ctm[3] - ctm[1] * ctm[2];
    if !det.is_finite() || det.abs() < 1.0e-12 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for r in rects {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])] {
            let (dx, dy) = (x - ctm[4], y - ctm[5]);
            let (u, v) = ((ctm[3] * dx - ctm[2] * dy) / det, (ctm[0] * dy - ctm[1] * dx) / det);
            let (px, py) = (u * w as f64, (1.0 - v) * h as f64);
            (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
        }
        if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) || x1 <= EDGE || y1 <= EDGE || x0 >= w as f64 - EDGE || y0 >= h as f64 - EDGE {
            continue;
        }
        let span = |lo: f64, hi: f64, n: usize| ((lo + EDGE).max(0.0).floor() as usize, ((hi - EDGE).max(0.0).ceil() as usize).min(n));
        let (x, y) = (span(x0, x1, w), span(y0, y1, h));
        if x.0 < x.1 && y.0 < y.1 {
            out.push(Region { x, y });
        }
    }
    out
}

/// The runs of pixels the marks cover, as (row, first column, one past the last column).
fn runs(regions: &[Region]) -> Vec<(usize, usize, usize)> {
    regions.iter().flat_map(|r| (r.y.0..r.y.1).map(move |y| (y, r.x.0, r.x.1))).collect()
}

/// Rows of `stride` bytes with the PNG "up" filter (7.4.4.4, predictor 12: every row's filter byte is 2), Flate-coded.
fn flate_up(data: &[u8], stride: usize) -> Vec<u8> {
    let mut filtered = Vec::with_capacity(data.len() + data.len() / stride.max(1) + 1);
    let mut above: &[u8] = &[];
    for row in data.chunks(stride.max(1)) {
        filtered.push(2);
        filtered.extend(row.iter().enumerate().map(|(i, &b)| b.wrapping_sub(above.get(i).copied().unwrap_or(0))));
        above = row;
    }
    compress_to_vec_zlib(&filtered, 3)
}

fn parms(colors: usize, bits: usize, columns: usize) -> Object {
    let mut p = Dict::new();
    p.set("Predictor", Object::Integer(12));
    p.set("Colors", Object::Integer(colors as i64));
    p.set("BitsPerComponent", Object::Integer(bits as i64));
    p.set("Columns", Object::Integer(columns as i64));
    Object::Dict(p)
}

/// A picture dictionary with what the new picture has of the old: its shape, its colours as 8-bit samples, the entries that do not touch the pixels.
fn picture_dict(old: &Dict, w: usize, h: usize, space: &str) -> Dict {
    let mut d = Dict::new();
    d.set("Type", Object::from("XObject"));
    d.set("Subtype", Object::from("Image"));
    d.set("Width", Object::Integer(w as i64));
    d.set("Height", Object::Integer(h as i64));
    d.set("ColorSpace", Object::from(space));
    d.set("BitsPerComponent", Object::Integer(8));
    for key in ["Interpolate", "Intent", "OC"] {
        if let Some(v) = old.get(key) {
            d.set(key, v.clone());
        }
    }
    d
}

fn spend(work: &Work, units: f64) -> Result<()> {
    if work.charge(units) {
        Ok(())
    } else if work.was_cancelled() {
        Err(Error::Cancelled)
    } else {
        Err(Error::Limit("redacting the pictures asks for more work than is allowed".to_string()))
    }
}

/// Paint the pixels of the picture `stream` that the marks `rects` cover with `color`. `ctm` is the matrix the picture is drawn under, `work` is charged
/// for what is done. `Gone` for a picture that cannot be decoded.
pub(super) fn redact_image(doc: &Document, work: &Work, stream: &Stream, ctm: &[f64; 6], rects: &[[f64; 4]], color: [u8; 3]) -> Result<Outcome> {
    let dim = |k: &str| stream.dict.get(k).and_then(Object::as_int).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
    let (w, h) = (dim("Width"), dim("Height"));
    if w == 0 || h == 0 || w.saturating_mul(h) > MAX_PIXELS {
        return Ok(Outcome::Gone);
    }
    if regions(ctm, rects, w, h).is_empty() {
        return Ok(Outcome::Clear);
    }
    let stencil = matches!(stream.dict.get("ImageMask"), Some(Object::Bool(true)));
    let jpeg_file = |d: &Dict| match d.get("Filter") {
        Some(Object::Name(n)) => n == "DCTDecode",
        Some(Object::Array(a)) => a.iter().any(|f| matches!(f, Object::Name(n) if n == "DCTDecode")),
        _ => false,
    };
    let Some(mut px) = decode_image(doc, stream, work, (w, h))? else { return Ok(Outcome::Gone) };
    let (pw, ph, ch) = (px.width as usize, px.height as usize, usize::from(px.channels));
    if pw.saturating_mul(ph).saturating_mul(ch) != px.data.len() {
        return Ok(Outcome::Gone);
    }
    let runs = runs(&regions(ctm, rects, pw, ph));
    if runs.is_empty() {
        return Ok(Outcome::Clear);
    }
    let covered: usize = runs.iter().map(|r| r.2 - r.1).sum();
    spend(work, (covered + pw * ph / 16) as f64 * PAINT_COST)?;

    if stencil {
        // The stencil: a pixel is painted where the opacity is on; under a mark none is. Bits, 0 for painted (the default /Decode).
        let stride = pw.div_ceil(8);
        let mut bits = vec![0xFFu8; stride * ph];
        for (i, row) in bits.chunks_mut(stride).enumerate() {
            for x in 0..pw {
                let painted = if ch == 4 { px.data.get((i * pw + x) * 4 + 3).is_some_and(|&a| a >= 128) } else { true };
                if painted && let Some(b) = row.get_mut(x / 8) {
                    *b &= !(0x80 >> (x % 8));
                }
            }
        }
        for &(y, x0, x1) in &runs {
            for x in x0..x1 {
                if let Some(b) = bits.get_mut(y * stride + x / 8) {
                    *b |= 0x80 >> (x % 8);
                }
            }
        }
        spend(work, bits.len() as f64 * cost::SQZ_FLATE_BYTE)?;
        let mut d = Dict::new();
        d.set("Type", Object::from("XObject"));
        d.set("Subtype", Object::from("Image"));
        d.set("Width", Object::Integer(pw as i64));
        d.set("Height", Object::Integer(ph as i64));
        d.set("ImageMask", Object::Bool(true));
        d.set("BitsPerComponent", Object::Integer(1));
        for key in ["Interpolate", "Intent", "OC"] {
            if let Some(v) = stream.dict.get(key) {
                d.set(key, v.clone());
            }
        }
        d.set("Filter", Object::from("FlateDecode"));
        d.set("DecodeParms", parms(1, 1, pw));
        return Ok(Outcome::Painted(Stream { dict: d, data: flate_up(&bits, stride) }, None));
    }

    // A grey picture painted with a colour is made colour first.
    let grey_paint = color[0] == color[1] && color[1] == color[2];
    if ch == 1 && !grey_paint {
        px.data = px.data.iter().flat_map(|&g| [g, g, g]).collect();
        px.channels = 3;
    }
    let ch = usize::from(px.channels);
    let paint: Vec<u8> = match ch {
        1 => vec![((u32::from(color[0]) * 299 + u32::from(color[1]) * 587 + u32::from(color[2]) * 114) / 1000) as u8],
        3 => color.to_vec(),
        _ => vec![color[0], color[1], color[2], 255],
    };
    for &(y, x0, x1) in &runs {
        for x in x0..x1 {
            let at = (y * pw + x) * ch;
            if let Some(slot) = px.data.get_mut(at..at + ch) {
                slot.copy_from_slice(&paint);
            }
        }
    }
    // Colour and opacity apart.
    let (samples, alpha) = if ch == 4 {
        let mut rgb = Vec::with_capacity(pw * ph * 3);
        let mut a = Vec::with_capacity(pw * ph);
        for &[r, g, b, alpha] in px.data.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[r, g, b]);
            a.push(alpha);
        }
        (rgb, Some(a))
    } else {
        (std::mem::take(&mut px.data), None)
    };
    let comps = if ch == 1 { 1 } else { 3 };
    let space = if comps == 1 { "DeviceGray" } else { "DeviceRGB" };
    let mut dict = picture_dict(&stream.dict, pw, ph, space);
    let as_jpeg = jpeg_file(&stream.dict) && pw * ph <= jpeg::MAX_PIXELS;
    spend(work, (pw * ph) as f64 * if as_jpeg { cost::SQZ_ENCODE_PIXEL } else { cost::SQZ_FLATE_BYTE * comps as f64 })?;
    let coded = if as_jpeg { jpeg::encode(&Raster { w: pw, h: ph, comps, data: &samples }, QUALITY).ok() } else { None };
    let data = match coded {
        Some(j) => {
            dict.set("Filter", Object::from("DCTDecode"));
            j
        }
        None => {
            dict.set("Filter", Object::from("FlateDecode"));
            dict.set("DecodeParms", parms(comps, 8, pw));
            flate_up(&samples, pw * comps)
        }
    };
    let mask = alpha.filter(|a| a.iter().any(|&v| v != 255)).map(|a| {
        let mut d = picture_dict(&Dict::new(), pw, ph, "DeviceGray");
        d.set("Filter", Object::from("FlateDecode"));
        d.set("DecodeParms", parms(1, 8, pw));
        Stream { dict: d, data: flate_up(&a, pw) }
    });
    Ok(Outcome::Painted(Stream { dict, data }, mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_covers_the_pixels_it_touches_and_not_the_ones_next_to_it() {
        // A picture of 10 by 10 pixels drawn 100 points wide at (50, 50): a pixel is 10 points.
        let ctm = [100.0, 0.0, 0.0, 100.0, 50.0, 50.0];
        let r = regions(&ctm, &[[70.0, 80.0, 90.0, 120.0]], 10, 10);
        assert_eq!(r.len(), 1);
        // Columns 2 and 3 (x 70 to 90); rows from the top: y 80 to 120 is v 0.3 to 0.7, rows 3 to 6.
        assert_eq!((r[0].x, r[0].y), ((2, 4), (3, 7)));
        let runs = runs(&r);
        assert_eq!(runs.len(), 4);
        assert!(runs.iter().all(|&(_, a, b)| (a, b) == (2, 4)));
        // A mark that is off the picture gives nothing; one that only touches its edge too.
        assert!(regions(&ctm, &[[0.0, 0.0, 50.0, 200.0]], 10, 10).is_empty());
        assert!(regions(&ctm, &[[0.0, 0.0, 49.0, 60.0]], 10, 10).is_empty());
    }

    #[test]
    fn a_turned_picture_is_covered_by_the_box_that_holds_the_mark() {
        // A quarter turn: the box is exact. Turned by 45 degrees: the box of the corners of the mark is painted, which is more than the mark.
        let ctm = [0.0, 100.0, -100.0, 0.0, 100.0, 0.0];
        let r = regions(&ctm, &[[30.0, 20.0, 60.0, 40.0]], 10, 10);
        assert_eq!(r.len(), 1);
        let n: usize = runs(&r).iter().map(|x| x.2 - x.1).sum();
        assert_eq!(n, 6, "3 columns of 2 rows (or the other way) of 10 points each");
        let c = std::f64::consts::FRAC_1_SQRT_2 * 100.0;
        let ctm = [c, c, -c, c, 100.0, 0.0];
        let r = regions(&ctm, &[[100.0, 40.0, 110.0, 50.0]], 20, 20);
        assert_eq!(r.len(), 1);
        assert!(runs(&r).iter().map(|x| x.2 - x.1).sum::<usize>() >= 4);
    }

    #[test]
    fn the_pixels_of_a_picture_under_a_mark_become_the_colour() {
        let mut b = crate::testutil::PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let data: Vec<u8> = (0..16 * 16).map(|i| (i % 251) as u8 | 1).collect();
        let doc = Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap();
        let mut dict = Dict::new();
        dict.set("Subtype", Object::from("Image"));
        dict.set("Width", Object::Integer(16));
        dict.set("Height", Object::Integer(16));
        dict.set("ColorSpace", Object::from("DeviceGray"));
        dict.set("BitsPerComponent", Object::Integer(8));
        let stream = Stream { dict, data: data.clone() };
        let ctm = [160.0, 0.0, 0.0, 160.0, 0.0, 0.0];
        let Outcome::Painted(image, mask) = redact_image(&doc, &Work::new(), &stream, &ctm, &[[40.0, 40.0, 80.0, 80.0]], [0, 0, 0]).unwrap() else { panic!("not painted") };
        assert!(mask.is_none());
        let back = doc.decode_stream(&image).unwrap();
        assert_eq!(back.len(), 256);
        // Columns 4 to 7 (x 40 to 80), rows 8 to 11 from the top (y 80 down to 40 is v 0.5 to 0.25).
        for y in 0..16 {
            for x in 0..16 {
                let under = (4..8).contains(&x) && (8..12).contains(&y);
                assert_eq!(back[y * 16 + x], if under { 0 } else { data[y * 16 + x] }, "{x},{y}");
            }
        }
        // A mark that misses the picture leaves it as it is.
        assert!(matches!(redact_image(&doc, &Work::new(), &stream, &ctm, &[[200.0, 200.0, 300.0, 300.0]], [0, 0, 0]).unwrap(), Outcome::Clear));
    }
}
