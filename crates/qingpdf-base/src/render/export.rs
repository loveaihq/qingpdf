//! An image of a page for another file format (5b, PDF to Word): the picture an image XObject shows, as plain pixels.

use std::cell::Cell;

use crate::document::Document;
use crate::error::Result;
use crate::object::Stream;
use crate::text::font::Warnings;

use super::func::Meter;
use super::image::{self, Env, Loaded};
use super::jbig2::GlobalsCache;
pub use super::pngout::{crc32_update, png};
use super::work::Work;

/// The pixels of an image, top row first, without padding: 1 byte a pixel for grey, 3 for colour, 4 (R, G, B, A, not premultiplied)
/// when some pixel is transparent.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub data: Vec<u8>,
}

/// Decode the image `stream` (an image XObject) to at most `target` pixels (width, height; a whole factor smaller than the image
/// where the image is bigger), charged to `work`. `Ok(None)`: the image cannot be decoded (a damaged stream, a filter we do not
/// have), or it is one of the blank placeholders. [`crate::Error::Limit`] and `Cancelled` stop the whole page.
pub fn decode_image(doc: &Document, stream: &Stream, work: &Work, target: (usize, usize)) -> Result<Option<Pixels>> {
    let lookup = |_: &[u8]| None;
    let meter = Meter::new(super::MAX_FUNCTION_STEPS);
    let pixels_left = Cell::new(super::interp::MAX_PAGE_IMAGE_PIXELS);
    let globals = GlobalsCache::default();
    let env = Env { doc, lookup: &lookup, fill: [0.0; 3], target, meter: &meter, pixels_left: &pixels_left, work, globals: &globals };
    let mut warnings = Warnings::default();
    let loaded = match image::load(&env, stream, &mut warnings) {
        Ok(l) => l,
        Err(e @ (crate::Error::Limit(_) | crate::Error::Cancelled)) => return Err(e),
        Err(_) => return Ok(None),
    };
    if work.was_cancelled() {
        return Err(crate::Error::Cancelled);
    }
    let Loaded::Image { pixmap, opaque } = loaded else { return Ok(None) };
    let (width, height) = (pixmap.width(), pixmap.height());
    let rgba = pixmap.data();
    let gray = rgba.chunks_exact(4).all(|p| matches!(p, [r, g, b, _] if r == g && g == b));
    let channels = if !opaque { 4 } else if gray { 1 } else { 3 };
    let mut data = Vec::with_capacity(rgba.len() / 4 * usize::from(channels));
    for p in rgba.chunks_exact(4) {
        if let [r, g, b, a] = *p {
            match channels {
                1 => data.push(r),
                3 => data.extend_from_slice(&[r, g, b]),
                _ => {
                    // (Premultiplied: divide the opacity out.)
                    let un = |c: u8| if a == 0 { 0 } else { u8::try_from((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).unwrap_or(255) };
                    data.extend_from_slice(&[un(r), un(g), un(b), a]);
                }
            }
        }
    }
    Ok(Some(Pixels { width, height, channels, data }))
}
