//! Reading just enough of JPEG and PNG files to put them on PDF pages
//! (images to PDF). JPEG data is embedded as it is, with `DCTDecode`
//! (ISO 32000-1 7.4.8); only the header is read. PNG is decoded with the
//! `png` crate and stored with `FlateDecode`, transparency as a soft mask
//! (11.6.5.2, 8.9.5).

use std::io::Cursor;

use crate::error::{Error, Result};
use crate::object::{Dict, ObjRef, Object};
use crate::writer::Builder;

/// A4 in points (595.276 x 841.89, ISO 216 rounded the way PDF tools do).
pub const A4_WIDTH: f64 = 595.276;
pub const A4_HEIGHT: f64 = 841.89;
/// Pixel density assumed when the file does not say (`--page fit`).
pub const DEFAULT_DPI: f64 = 96.0;
/// Page sides Annex C allows are 3 to 14400 units; fitted pages stay inside.
const MIN_PAGE_SIDE: f64 = 3.0;
const MAX_PAGE_SIDE: f64 = 14_400.0;
/// Decoded PNG pixels larger than this are refused instead of exhausting memory.
const MAX_PNG_BYTES: usize = 512 * 1024 * 1024;

/// How big the page is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageMode {
    /// A4, portrait or landscape to suit the image; the image is scaled to
    /// fit and centred.
    A4,
    /// The page is exactly the size of the image at its own pixel density
    /// (96 dpi if it has none).
    Fit,
}

/// The kinds of image file we take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
}

/// Which format the bytes are, from the signature (not the file name).
pub fn sniff(data: &[u8]) -> Option<Format> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Format::Png)
    } else {
        None
    }
}

// --- JPEG ---------------------------------------------------------------------

/// What the header of a JPEG file says.
#[derive(Debug, Clone, PartialEq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    /// 1 (gray), 3 (RGB) or 4 (CMYK).
    pub components: u8,
    /// The transform byte of the Adobe APP14 marker, if there is one. Files
    /// from Photoshop and similar store CMYK inverted and say so this way.
    pub adobe_transform: Option<u8>,
    /// EXIF orientation 1 to 8 (1 when there is none).
    pub orientation: u8,
    /// Pixels per inch (horizontal, vertical) from JFIF, when it gives a real
    /// density (units 1 = inch, 2 = centimetre; 0 only gives an aspect ratio).
    pub dpi: Option<(f64, f64)>,
}

fn be16(data: &[u8], pos: usize) -> Option<u16> {
    let b = data.get(pos..pos.checked_add(2)?)?;
    Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
}

fn bad_jpeg(why: &str) -> Error {
    Error::Invalid(format!("not a usable JPEG file: {why}"))
}

/// Read the size, colour components, density, orientation and Adobe marker
/// from a JPEG file. Walks the marker segments up to the start of the scan;
/// never reads image data.
pub fn parse_jpeg(data: &[u8]) -> Result<JpegInfo> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err(bad_jpeg("it does not start with the JPEG signature"));
    }
    let mut pos = 2usize;
    let mut frame: Option<(u8, u32, u32, u8)> = None; // precision, width, height, components
    let mut adobe = None;
    let mut orientation = 1u8;
    let mut dpi = None;
    let mut seen_exif = false;
    let mut seen_jfif = false;
    loop {
        // Markers are 0xFF, any number of 0xFF fill bytes, then the code.
        if data.get(pos) != Some(&0xFF) {
            return Err(bad_jpeg("damaged header segments"));
        }
        while data.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let Some(&marker) = data.get(pos) else {
            return Err(bad_jpeg("it ends inside the header"));
        };
        pos += 1;
        match marker {
            // Stuffed zero, TEM, RSTn, SOI: no length field.
            0x00 | 0x01 | 0xD0..=0xD8 => continue,
            // EOI, or SOS: the image data starts; the header is over.
            0xD9 | 0xDA => break,
            _ => {}
        }
        let len = usize::from(be16(data, pos).ok_or_else(|| bad_jpeg("it ends inside the header"))?);
        let end = pos.checked_add(len).ok_or_else(|| bad_jpeg("damaged segment length"))?;
        let segment = if len >= 2 { data.get(pos + 2..end) } else { None };
        let segment = segment.ok_or_else(|| bad_jpeg("a header segment is cut short"))?;
        match marker {
            // SOF0-2: baseline, extended sequential, progressive (all
            // Huffman-coded DCT, which DCTDecode reads).
            0xC0..=0xC2 if frame.is_none() => {
                let precision = *segment.first().ok_or_else(|| bad_jpeg("cut short"))?;
                let height = be16(segment, 1).ok_or_else(|| bad_jpeg("cut short"))?;
                let width = be16(segment, 3).ok_or_else(|| bad_jpeg("cut short"))?;
                let components = *segment.get(5).ok_or_else(|| bad_jpeg("cut short"))?;
                frame = Some((precision, u32::from(width), u32::from(height), components));
            }
            // Lossless, hierarchical and arithmetic-coded variants: PDF
            // readers do not decode them.
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(Error::Invalid(
                    "unsupported JPEG type (lossless, hierarchical or arithmetic coding): PDF cannot hold it"
                        .to_string(),
                ));
            }
            0xE0 if !seen_jfif && segment.starts_with(b"JFIF\0") => {
                seen_jfif = true;
                dpi = jfif_density(segment);
            }
            0xE1 if !seen_exif && segment.starts_with(b"Exif\0\0") => {
                seen_exif = true;
                orientation = exif_orientation(segment.get(6..).unwrap_or(&[])).unwrap_or(1);
            }
            // "Adobe", version, two flag words, then the colour transform byte.
            0xEE if adobe.is_none() && segment.starts_with(b"Adobe") => {
                adobe = Some(segment.get(11).copied().unwrap_or(0));
            }
            _ => {}
        }
        pos = end;
    }
    let (precision, width, height, components) = frame.ok_or_else(|| bad_jpeg("no image header found"))?;
    if precision != 8 {
        return Err(Error::Invalid(format!("unsupported JPEG: {precision} bits per sample (PDF takes 8)")));
    }
    if width == 0 || height == 0 {
        return Err(bad_jpeg("the image has no size"));
    }
    if !matches!(components, 1 | 3 | 4) {
        return Err(Error::Invalid(format!("unsupported JPEG: {components} colour components")));
    }
    Ok(JpegInfo { width, height, components, adobe_transform: adobe, orientation, dpi })
}

/// Density from a JFIF APP0 segment: `JFIF\0`, version (2), units, Xdensity,
/// Ydensity. Only inches and centimetres are real densities.
fn jfif_density(segment: &[u8]) -> Option<(f64, f64)> {
    let units = *segment.get(7)?;
    let x = f64::from(be16(segment, 8)?);
    let y = f64::from(be16(segment, 10)?);
    if x < 1.0 || y < 1.0 {
        return None;
    }
    match units {
        1 => Some((x, y)),
        2 => Some((x * 2.54, y * 2.54)),
        _ => None,
    }
}

/// The orientation tag (0x0112) of IFD0 in the TIFF structure of an Exif
/// segment (after `Exif\0\0`), when it is between 1 and 8.
fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |pos: usize| -> Option<u16> {
        let b = tiff.get(pos..pos.checked_add(2)?)?;
        let pair = [*b.first()?, *b.get(1)?];
        Some(if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) })
    };
    let u32_at = |pos: usize| -> Option<u32> {
        let b = tiff.get(pos..pos.checked_add(4)?)?;
        let quad = [*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?];
        Some(if little { u32::from_le_bytes(quad) } else { u32::from_be_bytes(quad) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let count = usize::from(u16_at(ifd)?);
    for i in 0..count.min(512) {
        let entry = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        if u16_at(entry)? != 0x0112 {
            continue;
        }
        // SHORT (3) or, from sloppy writers, LONG (4); one value, held in the entry.
        let value = match u16_at(entry + 2)? {
            3 => u32::from(u16_at(entry + 8)?),
            4 => u32_at(entry + 8)?,
            _ => return None,
        };
        return u8::try_from(value).ok().filter(|v| (1..=8).contains(v));
    }
    None
}

// --- PNG ----------------------------------------------------------------------

/// A PNG as 8-bit gray or RGB, with its alpha channel split off.
#[derive(Debug, Clone, PartialEq)]
pub struct PngImage {
    pub width: u32,
    pub height: u32,
    /// Gray (one byte per pixel) or RGB (three).
    pub gray: bool,
    pub pixels: Vec<u8>,
    /// One byte per pixel; `None` when the image is opaque everywhere.
    pub alpha: Option<Vec<u8>>,
    /// Pixels per inch from the `pHYs` chunk, when it has a unit.
    pub dpi: Option<(f64, f64)>,
}

fn bad_png(why: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("not a usable PNG file: {why}"))
}

/// Decode a PNG to 8-bit gray or RGB. Palette images are expanded, 16-bit
/// samples reduced to 8, `tRNS` transparency turned into alpha.
pub fn decode_png(data: &[u8]) -> Result<PngImage> {
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits { bytes: MAX_PNG_BYTES });
    let mut reader = decoder.read_info().map_err(bad_png)?;
    let size = reader.output_buffer_size().filter(|&n| n <= MAX_PNG_BYTES);
    let Some(size) = size else {
        return Err(Error::Limit("the PNG image is too large".to_string()));
    };
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).map_err(bad_png)?;
    if frame.bit_depth != png::BitDepth::Eight {
        return Err(bad_png("unexpected bit depth after conversion"));
    }
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1usize,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(bad_png("palette was not expanded")),
    };
    let pixel_count = usize::try_from(frame.width)
        .ok()
        .and_then(|w| usize::try_from(frame.height).ok().and_then(|h| w.checked_mul(h)))
        .ok_or_else(|| Error::Limit("the PNG image is too large".to_string()))?;
    let used = pixel_count.checked_mul(channels).ok_or_else(|| Error::Limit("the PNG image is too large".to_string()))?;
    let raw = buf.get(..used).ok_or_else(|| bad_png("the pixel data is shorter than the header says"))?;

    let has_alpha = matches!(frame.color_type, png::ColorType::GrayscaleAlpha | png::ColorType::Rgba);
    let gray = matches!(frame.color_type, png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha);
    let (pixels, alpha) = if has_alpha {
        let color_channels = channels - 1;
        let mut pixels = Vec::with_capacity(pixel_count.saturating_mul(color_channels));
        let mut alpha = Vec::with_capacity(pixel_count);
        for px in raw.chunks_exact(channels) {
            let (color, a) = px.split_at(color_channels);
            pixels.extend_from_slice(color);
            alpha.push(a.first().copied().unwrap_or(255));
        }
        // Nothing transparent anywhere: no mask needed.
        let opaque = alpha.iter().all(|&a| a == 255);
        (pixels, if opaque { None } else { Some(alpha) })
    } else {
        (raw.to_vec(), None)
    };

    let dpi = reader.info().pixel_dims.and_then(|p| match p.unit {
        png::Unit::Meter if p.xppu > 0 && p.yppu > 0 => Some((f64::from(p.xppu) * 0.0254, f64::from(p.yppu) * 0.0254)),
        _ => None,
    });
    Ok(PngImage { width: frame.width, height: frame.height, gray, pixels, alpha, dpi })
}

// --- placing an image on a page ---------------------------------------------------

/// A page size and the matrix (`a b c d e f`, 8.3.3) that draws the unit
/// square image so it appears upright and in place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub page_width: f64,
    pub page_height: f64,
    pub matrix: [f64; 6],
}

/// The matrix placing a stored image, whose EXIF orientation is `orientation`,
/// in a box of `w` x `h` (the size it has once displayed upright), with the
/// box's lower left corner at the origin.
///
/// Image space has its origin at the lower left of the stored image and the
/// image fills the unit square (8.9.3). EXIF orientation says how the stored
/// image must be turned to be upright: 2 mirrors it left to right, 3 turns it
/// half way round, 4 mirrors it top to bottom, 5 transposes it, 6 turns it 90
/// degrees clockwise, 7 transverses it and 8 turns it 90 degrees
/// counter-clockwise. Each row below was found by mapping the corners of the
/// stored image to where they end up.
pub fn orientation_matrix(orientation: u8, w: f64, h: f64) -> [f64; 6] {
    match orientation {
        2 => [-w, 0.0, 0.0, h, w, 0.0],
        3 => [-w, 0.0, 0.0, -h, w, h],
        4 => [w, 0.0, 0.0, -h, 0.0, h],
        5 => [0.0, -h, -w, 0.0, w, h],
        6 => [0.0, -h, w, 0.0, 0.0, h],
        7 => [0.0, h, w, 0.0, 0.0, 0.0],
        8 => [0.0, h, -w, 0.0, w, 0.0],
        _ => [w, 0.0, 0.0, h, 0.0, 0.0],
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Work out the page and the image matrix for an image of `width` x `height`
/// stored pixels with the given orientation and density.
pub fn place(width: u32, height: u32, orientation: u8, dpi: Option<(f64, f64)>, mode: PageMode) -> Placement {
    let turned = orientation >= 5;
    let (stored_w, stored_h) = (f64::from(width), f64::from(height));
    // Size in pixels once upright, and the density along those axes.
    let (disp_w, disp_h) = if turned { (stored_h, stored_w) } else { (stored_w, stored_h) };
    let (dpi_x, dpi_y) = dpi.unwrap_or((DEFAULT_DPI, DEFAULT_DPI));
    let (dpi_x, dpi_y) = if turned { (dpi_y, dpi_x) } else { (dpi_x, dpi_y) };

    match mode {
        PageMode::A4 => {
            let (pw, ph) = if disp_w > disp_h { (A4_HEIGHT, A4_WIDTH) } else { (A4_WIDTH, A4_HEIGHT) };
            let scale = (pw / disp_w).min(ph / disp_h);
            let (w, h) = (disp_w * scale, disp_h * scale);
            let mut matrix = orientation_matrix(orientation, w, h);
            matrix[4] += (pw - w) / 2.0;
            matrix[5] += (ph - h) / 2.0;
            Placement { page_width: pw, page_height: ph, matrix }
        }
        PageMode::Fit => {
            let mut w = disp_w / dpi_x * 72.0;
            let mut h = disp_h / dpi_y * 72.0;
            let longest = w.max(h);
            if longest > MAX_PAGE_SIDE {
                let k = MAX_PAGE_SIDE / longest;
                (w, h) = (w * k, h * k);
            } else if longest < MIN_PAGE_SIDE {
                let k = MIN_PAGE_SIDE / longest;
                (w, h) = (w * k, h * k);
            }
            let (w, h) = (round3(w), round3(h));
            Placement { page_width: w, page_height: h, matrix: orientation_matrix(orientation, w, h) }
        }
    }
}

// --- image objects ----------------------------------------------------------------

/// Add a JPEG as an image XObject, embedding the file's bytes as they are
/// (7.4.8, 8.9.5). Inverted CMYK (Adobe marker) gets a `/Decode` array that
/// undoes the inversion (8.9.5.2).
pub fn add_jpeg(b: &mut Builder<'_>, data: &[u8], info: &JpegInfo) -> Result<ObjRef> {
    let mut dict = Dict::new();
    dict.set("Type", Object::from("XObject"));
    dict.set("Subtype", Object::from("Image"));
    dict.set("Width", Object::Integer(i64::from(info.width)));
    dict.set("Height", Object::Integer(i64::from(info.height)));
    let space = match info.components {
        1 => "DeviceGray",
        3 => "DeviceRGB",
        _ => "DeviceCMYK",
    };
    dict.set("ColorSpace", Object::from(space));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::from("DCTDecode"));
    if info.components == 4 && info.adobe_transform.is_some() {
        let inverted = [1, 0, 1, 0, 1, 0, 1, 0].into_iter().map(Object::Integer).collect();
        dict.set("Decode", Object::Array(inverted));
    }
    b.add(&Object::Stream(crate::object::Stream { dict, data: data.to_vec() }))
}

/// Add a decoded PNG as an image XObject, Flate-compressed, with its alpha
/// channel as a soft mask image (`/SMask`).
pub fn add_png(b: &mut Builder<'_>, png: &PngImage) -> Result<ObjRef> {
    let size = |space: &str| {
        let mut dict = Dict::new();
        dict.set("Type", Object::from("XObject"));
        dict.set("Subtype", Object::from("Image"));
        dict.set("Width", Object::Integer(i64::from(png.width)));
        dict.set("Height", Object::Integer(i64::from(png.height)));
        dict.set("ColorSpace", Object::from(space));
        dict.set("BitsPerComponent", Object::Integer(8));
        dict
    };
    let mut dict = size(if png.gray { "DeviceGray" } else { "DeviceRGB" });
    if let Some(alpha) = &png.alpha {
        let mask = b.add_flate_stream(size("DeviceGray"), alpha)?;
        dict.set("SMask", Object::Ref(mask));
    }
    b.add_flate_stream(dict, &png.pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG-shaped byte string: signature, the given segments, then SOS.
    fn jpeg_with(segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        for (marker, body) in segments {
            out.extend_from_slice(&[0xFF, *marker]);
            out.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_be_bytes());
            out.extend_from_slice(body);
        }
        out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 1, 2, 3, 0xFF, 0xD9]);
        out
    }

    fn sof(marker: u8, precision: u8, w: u16, h: u16, comps: u8) -> (u8, Vec<u8>) {
        let mut body = vec![precision];
        body.extend_from_slice(&h.to_be_bytes());
        body.extend_from_slice(&w.to_be_bytes());
        body.push(comps);
        for i in 0..comps {
            body.extend_from_slice(&[i + 1, 0x11, 0]);
        }
        (marker, body)
    }

    fn jfif(units: u8, x: u16, y: u16) -> (u8, Vec<u8>) {
        let mut body = b"JFIF\0\x01\x01".to_vec();
        body.push(units);
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&y.to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        (0xE0, body)
    }

    fn exif(little: bool, orientation: u16) -> (u8, Vec<u8>) {
        let mut body = b"Exif\0\0".to_vec();
        let u16b = |v: u16| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        let u32b = |v: u32| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        body.extend_from_slice(if little { b"II" } else { b"MM" });
        body.extend_from_slice(&u16b(42));
        body.extend_from_slice(&u32b(8)); // IFD0 right after the header
        body.extend_from_slice(&u16b(2)); // two entries
        // Some other tag first (ImageWidth, LONG).
        body.extend_from_slice(&u16b(0x0100));
        body.extend_from_slice(&u16b(4));
        body.extend_from_slice(&u32b(1));
        body.extend_from_slice(&u32b(640));
        // Orientation, SHORT, one value left-justified in the 4-byte field.
        body.extend_from_slice(&u16b(0x0112));
        body.extend_from_slice(&u16b(3));
        body.extend_from_slice(&u32b(1));
        body.extend_from_slice(&u16b(orientation));
        body.extend_from_slice(&[0, 0]);
        (0xE1, body)
    }

    #[test]
    fn sniffing_goes_by_signature() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Format::Png));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn jpeg_size_components_and_defaults() {
        let data = jpeg_with(&[sof(0xC0, 8, 640, 480, 3)]);
        let info = parse_jpeg(&data).unwrap();
        assert_eq!(info, JpegInfo { width: 640, height: 480, components: 3, adobe_transform: None, orientation: 1, dpi: None });
        // Progressive and extended sequential are accepted too.
        for marker in [0xC1, 0xC2] {
            assert_eq!(parse_jpeg(&jpeg_with(&[sof(marker, 8, 10, 20, 1)])).unwrap().components, 1);
        }
        assert_eq!(parse_jpeg(&jpeg_with(&[sof(0xC0, 8, 1, 1, 4)])).unwrap().components, 4);
    }

    #[test]
    fn jpeg_header_problems_are_errors() {
        assert!(parse_jpeg(b"").is_err());
        assert!(parse_jpeg(b"\xFF\xD8").is_err());
        assert!(parse_jpeg(b"not a jpeg at all").is_err());
        // No frame header before the scan.
        assert!(parse_jpeg(&jpeg_with(&[jfif(1, 72, 72)])).is_err());
        // Twelve-bit, two components, zero size, lossless, arithmetic.
        assert!(parse_jpeg(&jpeg_with(&[sof(0xC0, 12, 10, 10, 1)])).is_err());
        assert!(parse_jpeg(&jpeg_with(&[sof(0xC0, 8, 10, 10, 2)])).is_err());
        assert!(parse_jpeg(&jpeg_with(&[sof(0xC0, 8, 0, 10, 1)])).is_err());
        assert!(matches!(parse_jpeg(&jpeg_with(&[sof(0xC3, 8, 10, 10, 1)])), Err(Error::Invalid(_))));
        assert!(matches!(parse_jpeg(&jpeg_with(&[sof(0xC9, 8, 10, 10, 1)])), Err(Error::Invalid(_))));
        // A segment longer than the file.
        let mut cut = jpeg_with(&[sof(0xC0, 8, 10, 10, 3)]);
        cut.truncate(12);
        assert!(parse_jpeg(&cut).is_err());
        // Every prefix of a good file is safe.
        let good = jpeg_with(&[jfif(1, 72, 72), exif(true, 6), sof(0xC0, 8, 10, 10, 3)]);
        for n in 0..good.len() {
            let _ = parse_jpeg(&good[..n]);
        }
    }

    #[test]
    fn jfif_density_units() {
        let dpi = |units, x, y| parse_jpeg(&jpeg_with(&[jfif(units, x, y), sof(0xC0, 8, 4, 4, 3)])).unwrap().dpi;
        assert_eq!(dpi(1, 300, 300), Some((300.0, 300.0)));
        let (x, y) = dpi(2, 100, 200).unwrap();
        assert!((x - 254.0).abs() < 1e-9 && (y - 508.0).abs() < 1e-9);
        assert_eq!(dpi(0, 1, 1), None); // aspect ratio only
        assert_eq!(dpi(1, 0, 72), None);
    }

    #[test]
    fn exif_orientation_both_byte_orders() {
        for little in [true, false] {
            for o in 1..=8u16 {
                let data = jpeg_with(&[exif(little, o), sof(0xC0, 8, 4, 4, 3)]);
                assert_eq!(parse_jpeg(&data).unwrap().orientation, o as u8, "little = {little}, o = {o}");
            }
            // Out of range values count as upright.
            for o in [0u16, 9, 300] {
                let data = jpeg_with(&[exif(little, o), sof(0xC0, 8, 4, 4, 3)]);
                assert_eq!(parse_jpeg(&data).unwrap().orientation, 1);
            }
        }
        // Garbage Exif is ignored, not an error.
        let data = jpeg_with(&[(0xE1, b"Exif\0\0II\x2A\x00\xFF\xFF\xFF\xFF".to_vec()), sof(0xC0, 8, 4, 4, 3)]);
        assert_eq!(parse_jpeg(&data).unwrap().orientation, 1);
        let data = jpeg_with(&[(0xE1, b"Exif\0\0".to_vec()), sof(0xC0, 8, 4, 4, 3)]);
        assert_eq!(parse_jpeg(&data).unwrap().orientation, 1);
    }

    #[test]
    fn adobe_marker_is_noticed() {
        let mut adobe = b"Adobe\0\x64\x00\x00\x00\x00".to_vec();
        adobe.push(2); // YCCK
        let data = jpeg_with(&[(0xEE, adobe), sof(0xC0, 8, 4, 4, 4)]);
        let info = parse_jpeg(&data).unwrap();
        assert_eq!(info.adobe_transform, Some(2));
        assert_eq!(info.components, 4);
        assert_eq!(parse_jpeg(&jpeg_with(&[sof(0xC0, 8, 4, 4, 4)])).unwrap().adobe_transform, None);
    }

    /// Map the corners of a `w` x `h` stored image's unit square through the
    /// matrix, as (x, y) for stored (0,0) (1,0) (0,1) (1,1).
    fn corners(m: [f64; 6]) -> [(i64, i64); 4] {
        let at = |u: f64, v: f64| {
            ((m[0] * u + m[2] * v + m[4]).round() as i64, (m[1] * u + m[3] * v + m[5]).round() as i64)
        };
        [at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0), at(1.0, 1.0)]
    }

    #[test]
    fn orientation_matrices_put_the_corners_where_exif_says() {
        // Upright box 30 wide, 20 high. Corners listed in the order
        // stored bottom-left, bottom-right, top-left, top-right; the answer is
        // where each lands in the upright box, which has its own
        // bottom-left (0,0), bottom-right (30,0), top-left (0,20), top-right (30,20).
        let (bl, br, tl, tr) = ((0, 0), (30, 0), (0, 20), (30, 20));
        let cases: [(u8, [(i64, i64); 4]); 8] = [
            (1, [bl, br, tl, tr]),
            // 2: mirrored left to right.
            (2, [br, bl, tr, tl]),
            // 3: turned half way.
            (3, [tr, tl, br, bl]),
            // 4: mirrored top to bottom.
            (4, [tl, tr, bl, br]),
            // 5: row 0 is the visual left, column 0 the visual top: the
            // stored top-left stays top-left, stored top-right goes to the
            // bottom-left, stored bottom-left goes to the top-right.
            (5, [tr, br, tl, bl]),
            // 6: row 0 is the visual right, column 0 the visual top: stored
            // top-left -> top-right, top-right -> bottom-right, bottom-left -> top-left.
            (6, [tl, bl, tr, br]),
            // 7: row 0 is the visual right, column 0 the visual bottom: stored
            // top-left -> bottom-right, top-right -> top-right, bottom-left -> bottom-left.
            (7, [bl, tl, br, tr]),
            // 8: row 0 is the visual left, column 0 the visual bottom: stored
            // top-left -> bottom-left, top-right -> top-left, bottom-left -> bottom-right.
            (8, [br, tr, bl, tl]),
        ];
        for (o, expected) in cases {
            assert_eq!(corners(orientation_matrix(o, 30.0, 20.0)), expected, "orientation {o}");
        }
        // Anything else is upright.
        assert_eq!(orientation_matrix(0, 3.0, 2.0), orientation_matrix(1, 3.0, 2.0));
        assert_eq!(orientation_matrix(9, 3.0, 2.0), orientation_matrix(1, 3.0, 2.0));
    }

    #[test]
    fn a4_placement_portrait_landscape_and_centring() {
        // A wide image gets a landscape page and fills its width.
        let p = place(2000, 1000, 1, None, PageMode::A4);
        assert_eq!((p.page_width, p.page_height), (A4_HEIGHT, A4_WIDTH));
        assert!((p.matrix[0] - A4_HEIGHT).abs() < 1e-9);
        assert!((p.matrix[3] - A4_HEIGHT / 2.0).abs() < 1e-9);
        assert!(p.matrix[4].abs() < 1e-9);
        assert!((p.matrix[5] - (A4_WIDTH - A4_HEIGHT / 2.0) / 2.0).abs() < 1e-9);
        // A tall one gets portrait and fills the height, centred sideways.
        let p = place(500, 2000, 1, None, PageMode::A4);
        assert_eq!((p.page_width, p.page_height), (A4_WIDTH, A4_HEIGHT));
        assert!((p.matrix[3] - A4_HEIGHT).abs() < 1e-9);
        assert!((p.matrix[0] - A4_HEIGHT / 4.0).abs() < 1e-9);
        assert!((p.matrix[4] - (A4_WIDTH - A4_HEIGHT / 4.0) / 2.0).abs() < 1e-9);
        // A small image is scaled up; a square one is portrait.
        let p = place(10, 10, 1, None, PageMode::A4);
        assert_eq!((p.page_width, p.page_height), (A4_WIDTH, A4_HEIGHT));
        assert!((p.matrix[0] - A4_WIDTH).abs() < 1e-9);
        // A phone photo stored sideways (4000x3000, orientation 6) is upright
        // portrait: the page is portrait and the image fills its width.
        let p = place(4000, 3000, 6, None, PageMode::A4);
        assert_eq!((p.page_width, p.page_height), (A4_WIDTH, A4_HEIGHT));
        let w = A4_WIDTH;
        let h = A4_WIDTH * 4000.0 / 3000.0;
        assert!((p.matrix[2] - w).abs() < 1e-9, "x extent is the matrix c entry when turned");
        assert!((p.matrix[1] + h).abs() < 1e-9);
    }

    #[test]
    fn fit_placement_uses_density_default_96_and_the_page_limits() {
        // 96 dpi default: 960 px is 720 pt.
        let p = place(960, 480, 1, None, PageMode::Fit);
        assert_eq!((p.page_width, p.page_height), (720.0, 360.0));
        assert_eq!(p.matrix, [720.0, 0.0, 0.0, 360.0, 0.0, 0.0]);
        // The file's own density wins, per axis.
        let p = place(600, 600, 1, Some((300.0, 150.0)), PageMode::Fit);
        assert_eq!((p.page_width, p.page_height), (144.0, 288.0));
        // Turned images swap the axes, density included.
        let p = place(600, 300, 6, Some((300.0, 150.0)), PageMode::Fit);
        assert_eq!((p.page_width, p.page_height), (144.0, 144.0));
        // Huge and tiny images are kept within 3..14400.
        let p = place(100_000, 50_000, 1, Some((1.0, 1.0)), PageMode::Fit);
        assert_eq!((p.page_width, p.page_height), (14_400.0, 7_200.0));
        let p = place(1, 1, 1, Some((1000.0, 1000.0)), PageMode::Fit);
        assert_eq!((p.page_width, p.page_height), (3.0, 3.0));
    }

    fn make_png(width: u32, height: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, width, height);
            enc.set_color(color);
            enc.set_depth(depth);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn png_rgb_without_alpha_has_no_mask() {
        let data = make_png(2, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[1, 2, 3, 4, 5, 6]);
        let img = decode_png(&data).unwrap();
        assert_eq!((img.width, img.height, img.gray), (2, 1, false));
        assert_eq!(img.pixels, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(img.alpha, None);
        assert_eq!(img.dpi, None);
    }

    #[test]
    fn png_alpha_becomes_a_separate_channel() {
        let rgba = make_png(2, 1, png::ColorType::Rgba, png::BitDepth::Eight, &[10, 20, 30, 255, 40, 50, 60, 128]);
        let img = decode_png(&rgba).unwrap();
        assert!(!img.gray);
        assert_eq!(img.pixels, vec![10, 20, 30, 40, 50, 60]);
        assert_eq!(img.alpha, Some(vec![255, 128]));
        let ga = make_png(2, 1, png::ColorType::GrayscaleAlpha, png::BitDepth::Eight, &[7, 0, 9, 200]);
        let img = decode_png(&ga).unwrap();
        assert!(img.gray);
        assert_eq!(img.pixels, vec![7, 9]);
        assert_eq!(img.alpha, Some(vec![0, 200]));
        // Fully opaque alpha is dropped.
        let opaque = make_png(1, 2, png::ColorType::Rgba, png::BitDepth::Eight, &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(decode_png(&opaque).unwrap().alpha, None);
    }

    #[test]
    fn png_sixteen_bit_and_low_depth_are_brought_to_eight() {
        // 16-bit gray: keep the high byte.
        let g16 = make_png(2, 1, png::ColorType::Grayscale, png::BitDepth::Sixteen, &[0x12, 0x34, 0xAB, 0xCD]);
        let img = decode_png(&g16).unwrap();
        assert!(img.gray);
        assert_eq!(img.pixels, vec![0x12, 0xAB]);
        // 1-bit gray: 8 pixels in one byte, 0b10100000 -> white black white black ...
        let g1 = make_png(4, 1, png::ColorType::Grayscale, png::BitDepth::One, &[0b1010_0000]);
        let img = decode_png(&g1).unwrap();
        assert_eq!(img.pixels, vec![255, 0, 255, 0]);
    }

    #[test]
    fn png_palette_and_transparency_chunk() {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_palette(vec![255, 0, 0, 0, 255, 0]);
            enc.set_trns(vec![255, 0]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0, 1]).unwrap();
        }
        let img = decode_png(&out).unwrap();
        assert_eq!(img.pixels, vec![255, 0, 0, 0, 255, 0]);
        assert_eq!(img.alpha, Some(vec![255, 0]));
    }

    #[test]
    fn png_density_from_phys() {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 1, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_pixel_dims(Some(png::PixelDimensions { xppu: 11811, yppu: 5906, unit: png::Unit::Meter }));
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0]).unwrap();
        }
        let (x, y) = decode_png(&out).unwrap().dpi.unwrap();
        assert!((x - 300.0).abs() < 0.1 && (y - 150.0).abs() < 0.1, "{x} {y}");
        // Unit unspecified gives no density.
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 1, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_pixel_dims(Some(png::PixelDimensions { xppu: 1, yppu: 1, unit: png::Unit::Unspecified }));
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0]).unwrap();
        }
        assert_eq!(decode_png(&out).unwrap().dpi, None);
    }

    #[test]
    fn broken_png_is_an_error() {
        assert!(decode_png(b"").is_err());
        assert!(decode_png(b"\x89PNG\r\n\x1a\nnonsense").is_err());
        let good = make_png(3, 3, png::ColorType::Rgb, png::BitDepth::Eight, &[7; 27]);
        for n in [10, 20, 40, good.len() - 25] {
            assert!(decode_png(&good[..n]).is_err(), "prefix of {n} bytes");
        }
    }
}
