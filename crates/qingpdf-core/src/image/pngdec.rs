//! PNG files read (the PNG specification, third edition; RFC 2083 for the filters): the chunks, the zlib stream of the image data and
//! the filters of its rows, with Adam7 interlacing. It gives what `png::Transformations::normalize_to_color8()` of the `png` crate
//! gave, which the images of a PDF are made from: every sample as one byte (16 bits cut to the high byte, 1, 2 and 4 bits
//! spread over 0 to 255), palettes spelled out, and a transparent colour or a palette with alpha turned into an alpha channel.
//!
//! A file is read as the crate read it: the checksums of the chunks that are read must be right (an ancillary chunk with a wrong
//! one is left out, a critical chunk with a wrong one is an error), a transparency chunk or a pixel size chunk that does not fit
//! the image is left out, a critical chunk that is not known is an error, the chunks after the image data are not looked at, and
//! the Adler-32 checksum of the zlib stream is not checked.

use std::cell::Cell;

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

use super::{MAX_INTERLACED_BYTES, PNG_WORKING_BYTES, bad_png, png_too_large};
use crate::error::{Error, Result};
use crate::filter::paeth;
use crate::render::export::crc32_update;

const SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

/// The place of the pixels of each of the seven passes of Adam7: the column and the row of the first, and the steps between them.
const ADAM7: [(usize, usize, usize, usize); 7] = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

/// The size of the window the zlib stream is read through (a power of two, at least 32 KiB).
const WINDOW: usize = 1 << 16;

/// What the chunks before the image data say, and where the image data is.
pub(super) struct Png<'a> {
    pub(super) width: u32,
    pub(super) height: u32,
    depth: u8,
    /// 0 grey, 2 RGB, 3 palette, 4 grey and alpha, 6 RGB and alpha.
    color: u8,
    interlaced: bool,
    /// The colours of a palette image, with their alpha; the places past the end of the palette are opaque black.
    palette: Box<[[u8; 4]; 256]>,
    /// A palette image has a transparency chunk (its alpha is in the `palette`).
    palette_alpha: bool,
    /// The transparent colour of a grey or RGB image, as the pixels are compared with it: the low bytes of its samples when
    /// the depth is below 16, and the chunk as it is when it is 16.
    key: Option<Vec<u8>>,
    /// Pixels per inch, from the pixel size chunk, when it has a unit.
    pub(super) dpi: Option<(f64, f64)>,
    /// The bodies of the IDAT chunks.
    idat: Vec<&'a [u8]>,
}

fn be32(bytes: &[u8]) -> u32 {
    bytes.iter().take(4).fold(0, |n, &b| n << 8 | u32::from(b))
}

/// Reads the chunks up to the end of the image data (the pixels are not looked at); with `header_only`, up to the start of it.
pub(super) fn read(data: &[u8], header_only: bool) -> Result<Png<'_>> {
    if !data.starts_with(&SIGNATURE) {
        return Err(bad_png("the signature is not that of a PNG file"));
    }
    let mut at = SIGNATURE.len();
    let mut png: Option<Png<'_>> = None;
    let mut palette_len: Option<usize> = None;
    let mut phys_seen = false;
    let mut trns_seen = false;
    let mut idat: Vec<&[u8]> = Vec::new();
    loop {
        let head = at.checked_add(8).and_then(|end| data.get(at..end));
        let Some((head, kind)) = head.and_then(|h| Some((h, <[u8; 4]>::try_from(h.get(4..8)?).ok()?))) else {
            if idat.is_empty() {
                return Err(bad_png("the file ends before the image data"));
            }
            break;
        };
        // The image data is the IDAT chunks one after the other; what comes after them is not looked at.
        if !idat.is_empty() && kind != *b"IDAT" || header_only && kind == *b"IDAT" {
            break;
        }
        let len = usize::try_from(be32(head)).map_err(|_| png_too_large())?;
        let body_at = at + 8;
        let body = body_at.checked_add(len).and_then(|end| data.get(body_at..end)).ok_or_else(|| bad_png("a chunk is cut short"))?;
        let stored = data.get(body_at + len..body_at + len + 4).map(be32);
        let crc_ok = stored == Some(crc32_update(crc32_update(0, &kind), body));
        at = body_at + len + 4;
        if png.is_none() && kind != *b"IHDR" {
            return Err(bad_png("the first chunk is not the header"));
        }
        match &kind {
            b"IHDR" => {
                if png.is_some() || len != 13 || !crc_ok {
                    return Err(bad_png("the header is not right"));
                }
                png = Some(header(body)?);
            }
            b"PLTE" => {
                // (A palette that is not made of whole colours is an error only for an image that is made of its colours.)
                let broken = len % 3 != 0 && png.as_ref().is_some_and(|p| p.color == 3);
                if palette_len.is_some() || !(3..=768).contains(&len) || broken || !crc_ok {
                    return Err(bad_png("the palette is not right"));
                }
                palette_len = Some(len);
                if let Some(p) = png.as_mut() {
                    for (slot, &[r, g, b]) in p.palette.iter_mut().zip(body.as_chunks::<3>().0) {
                        *slot = [r, g, b, 255];
                    }
                }
            }
            b"tRNS" if crc_ok && !trns_seen && (1..=256).contains(&len) => {
                if let Some(p) = png.as_mut() {
                    trns_seen = p.transparency(body, palette_len);
                }
            }
            b"pHYs" if crc_ok && !phys_seen && len == 9 => {
                let unit = body.get(8).copied().unwrap_or(2);
                if let Some(p) = png.as_mut().filter(|_| unit < 2) {
                    phys_seen = true;
                    let (x, y) = (be32(body), be32(body.get(4..).unwrap_or(&[])));
                    p.dpi = (unit == 1 && x > 0 && y > 0).then(|| (f64::from(x) * 0.0254, f64::from(y) * 0.0254));
                }
            }
            b"IDAT" => {
                if !crc_ok {
                    return Err(bad_png("the checksum of the image data is wrong"));
                }
                idat.push(body);
            }
            b"IEND" => return Err(bad_png("there is no image data")),
            // (The ones that are not looked at are the ancillary chunks; a critical one that is not known cannot be read past.)
            _ if kind[0] & 0x20 == 0 => return Err(bad_png("the file has a chunk that has to be understood and is not")),
            _ => {}
        }
    }
    let mut png = png.ok_or_else(|| bad_png("the file has no header"))?;
    if !header_only {
        if idat.is_empty() {
            return Err(bad_png("there is no image data"));
        }
        if png.color == 3 && palette_len.is_none() {
            return Err(bad_png("the image needs a palette and has none"));
        }
    }
    png.idat = idat;
    Ok(png)
}

/// The header (13 bytes) of an image.
fn header<'a>(body: &[u8]) -> Result<Png<'a>> {
    let byte = |i: usize| body.get(i).copied().unwrap_or(0);
    let (width, height, depth, color) = (be32(body), be32(body.get(4..).unwrap_or(&[])), byte(8), byte(9));
    if width == 0 || height == 0 {
        return Err(bad_png("the image has no size"));
    }
    let depths: &[u8] = match color {
        0 => &[1, 2, 4, 8, 16],
        2 | 4 | 6 => &[8, 16],
        3 => &[1, 2, 4, 8],
        _ => return Err(bad_png("unknown colour type")),
    };
    if !depths.contains(&depth) {
        return Err(bad_png("the bit depth is not one the colour type has"));
    }
    if byte(10) != 0 || byte(11) != 0 || byte(12) > 1 {
        return Err(bad_png("unknown compression, filter or interlace method"));
    }
    Ok(Png {
        width,
        height,
        depth,
        color,
        interlaced: byte(12) == 1,
        palette: Box::new([[0, 0, 0, 255]; 256]),
        palette_alpha: false,
        key: None,
        dpi: None,
        idat: Vec::new(),
    })
}

impl Png<'_> {
    /// Takes the transparency chunk `body` (`palette`: the bytes of the palette, if one came before). Whether it was taken.
    fn transparency(&mut self, body: &[u8], palette: Option<usize>) -> bool {
        let low = |i: usize| body.get(i).copied().unwrap_or(0);
        match self.color {
            0 if body.len() >= 2 => self.key = Some(if self.depth < 16 { vec![low(1)] } else { body.to_vec() }),
            2 if body.len() >= 6 => self.key = Some(if self.depth < 16 { vec![low(1), low(3), low(5)] } else { body.to_vec() }),
            3 => {
                let Some(entries) = palette.map(|bytes| bytes / 3) else { return false };
                // (More alpha values than colours: the chunk is taken to be empty.)
                if body.len() <= entries {
                    for (slot, &alpha) in self.palette.iter_mut().zip(body) {
                        slot[3] = alpha;
                    }
                }
                self.palette_alpha = true;
            }
            _ => return false,
        }
        true
    }

    /// Samples a pixel has as the file has them.
    fn channels_in(&self) -> usize {
        match self.color {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }

    /// Samples a pixel has as [`Png::rows`] gives it: grey, grey and alpha, RGB, RGBA.
    pub(super) fn channels(&self) -> usize {
        match self.color {
            0 => 1 + usize::from(self.key.is_some()),
            2 => 3 + usize::from(self.key.is_some()),
            3 => 3 + usize::from(self.palette_alpha),
            4 => 2,
            _ => 4,
        }
    }

    /// The pixels are grey (and not RGB).
    pub(super) fn gray(&self) -> bool {
        self.color == 0 || self.color == 4
    }

    /// The bytes of a row of `width` pixels in the file (without the byte of the filter).
    fn row_bytes(&self, width: usize) -> usize {
        (width * self.channels_in() * usize::from(self.depth)).div_ceil(8)
    }

    /// One row of unfiltered bytes as a row of pixels of 8-bit samples, put on the end of `out`.
    fn expand(&self, raw: &[u8], width: usize, out: &mut Vec<u8>) {
        let depth = usize::from(self.depth);
        let sample = |x: usize| {
            let bit = x * depth;
            let shift = 8 - depth - bit % 8;
            usize::from(raw.get(bit / 8).copied().unwrap_or(0) >> shift) & ((1 << depth) - 1)
        };
        if self.color == 3 {
            for x in 0..width {
                let colour = self.palette.get(sample(x)).copied().unwrap_or([0, 0, 0, 255]);
                out.extend_from_slice(colour.get(..self.channels()).unwrap_or(&[]));
            }
        } else if depth < 8 {
            let scale = 255 / ((1u16 << depth) - 1) as u8;
            for x in 0..width {
                let v = sample(x);
                out.push(v as u8 * scale);
                if let Some(key) = &self.key {
                    out.push(if key.first() == Some(&(v as u8)) { 0 } else { 255 });
                }
            }
        } else {
            let (bytes, samples) = (depth / 8, width * self.channels_in());
            match &self.key {
                None if bytes == 1 => out.extend_from_slice(raw.get(..samples).unwrap_or(raw)),
                None => out.extend(raw.iter().step_by(bytes).take(samples)),
                Some(key) => {
                    for pixel in raw.chunks_exact(self.channels_in() * bytes).take(width) {
                        out.extend(pixel.iter().step_by(bytes));
                        out.push(if pixel == key.as_slice() { 0 } else { 255 });
                    }
                }
            }
        }
    }

    /// Gives the rows of the image one after the other, from the top, to `push`. A file that is not interlaced is read a row at
    /// a time; an interlaced one has to be held whole (at most [`MAX_INTERLACED_BYTES`]) while its passes are put together.
    pub(super) fn rows(&self, mut push: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let (width, height) = (usize::try_from(self.width).map_err(|_| png_too_large())?, usize::try_from(self.height).map_err(|_| png_too_large())?);
        let channels = self.channels();
        let line = width.checked_mul(channels).ok_or_else(png_too_large)?;
        let row_bytes = self.row_bytes(width);
        // What is held while it is read: the row and the one before it, the row of pixels, the rows `push` may keep, the window.
        let working = row_bytes.saturating_add(1).saturating_mul(2).saturating_add(line.saturating_mul(3)).saturating_add(WINDOW);
        if working > PNG_WORKING_BYTES {
            return Err(png_too_large());
        }
        let bpp = (self.channels_in() * usize::from(self.depth)).div_ceil(8);
        let mut inflater = Inflater::new(&self.idat);
        let mut previous = vec![0u8; row_bytes];
        let mut current = vec![0u8; row_bytes];
        let mut pixels = Vec::with_capacity(line);
        if !self.interlaced {
            for _ in 0..height {
                inflater.row(&mut current, &previous, bpp)?;
                pixels.clear();
                self.expand(&current, width, &mut pixels);
                push(&pixels)?;
                std::mem::swap(&mut previous, &mut current);
            }
            return Ok(());
        }
        let size = line.checked_mul(height).filter(|&n| n <= MAX_INTERLACED_BYTES).ok_or_else(|| {
            Error::Limit(format!("the interlaced PNG image needs more than {MAX_INTERLACED_BYTES} bytes to decode"))
        })?;
        let mut image = vec![0u8; size];
        for (x0, y0, dx, dy) in ADAM7 {
            let (pass_width, pass_height) = (width.saturating_sub(x0).div_ceil(dx), height.saturating_sub(y0).div_ceil(dy));
            if pass_width == 0 || pass_height == 0 {
                continue;
            }
            let bytes = self.row_bytes(pass_width);
            previous.clear();
            previous.resize(bytes, 0);
            current.resize(bytes, 0);
            for r in 0..pass_height {
                inflater.row(&mut current, &previous, bpp)?;
                pixels.clear();
                self.expand(&current, pass_width, &mut pixels);
                for (i, pixel) in pixels.chunks_exact(channels).enumerate() {
                    let at = ((y0 + r * dy) * width + x0 + i * dx) * channels;
                    if let Some(place) = image.get_mut(at..at + channels) {
                        place.copy_from_slice(pixel);
                    }
                }
                std::mem::swap(&mut previous, &mut current);
            }
        }
        image.chunks_exact(line.max(1)).try_for_each(push)
    }
}

/// The zlib stream made of the IDAT chunks, read through a window as far as it is wanted.
struct Inflater<'a> {
    state: Box<DecompressorOxide>,
    /// The chunks that have not been started.
    rest: std::slice::Iter<'a, &'a [u8]>,
    /// What is left of the chunk that has.
    input: &'a [u8],
    window: Vec<u8>,
    /// Where the next output goes.
    at: usize,
    /// Output that has not been given out yet.
    ready: std::ops::Range<usize>,
    done: bool,
}

impl<'a> Inflater<'a> {
    fn new(chunks: &'a [&'a [u8]]) -> Inflater<'a> {
        Inflater { state: Box::default(), rest: chunks.iter(), input: &[], window: vec![0; WINDOW], at: 0, ready: 0..0, done: false }
    }

    /// Fills `out`, or says why it cannot.
    fn read(&mut self, out: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        while filled < out.len() {
            if self.ready.start < self.ready.end {
                let n = (self.ready.end - self.ready.start).min(out.len() - filled);
                if let (Some(from), Some(to)) = (self.window.get(self.ready.start..self.ready.start + n), out.get_mut(filled..filled + n)) {
                    to.copy_from_slice(from);
                }
                self.ready.start += n;
                filled += n;
            } else {
                self.step()?;
            }
        }
        Ok(())
    }

    /// Inflates a piece more into the window.
    fn step(&mut self) -> Result<()> {
        if self.done {
            return Err(bad_png("the pixel data is shorter than the header says"));
        }
        while self.input.is_empty() {
            match self.rest.next() {
                Some(chunk) => self.input = chunk,
                None => break,
            }
        }
        let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
            | inflate_flags::TINFL_FLAG_IGNORE_ADLER32
            | if self.rest.len() > 0 { inflate_flags::TINFL_FLAG_HAS_MORE_INPUT } else { 0 };
        let (status, used, written) = decompress(&mut self.state, self.input, &mut self.window, self.at, flags);
        self.input = self.input.get(used..).unwrap_or(&[]);
        self.ready = self.at..self.at + written;
        self.at = (self.at + written) % WINDOW;
        match status {
            TINFLStatus::Done => self.done = true,
            // (Running out of input or of room is fine as long as something moved.)
            TINFLStatus::NeedsMoreInput | TINFLStatus::HasMoreOutput if used > 0 || written > 0 => {}
            TINFLStatus::FailedCannotMakeProgress | TINFLStatus::NeedsMoreInput => return Err(bad_png("the pixel data is shorter than the header says")),
            _ => return Err(bad_png("the pixel data cannot be decompressed")),
        }
        Ok(())
    }

    /// Reads the next row of the image into `row`, with its filter undone (`previous`: the row above, unfiltered).
    fn row(&mut self, row: &mut [u8], previous: &[u8], bpp: usize) -> Result<()> {
        let mut tag = [0u8];
        self.read(&mut tag)?;
        self.read(row)?;
        unfilter(tag[0], row, previous, bpp)
    }
}

/// Undoes the filter `tag` (0 to 4: none, sub, up, average, Paeth) of a row, in place; `previous` is the row above it, already
/// unfiltered, and as long as `row`. (The bytes are cells so that one can be changed while the ones before it are read, with no
/// indexing; a window of `bpp + 1` bytes is a byte and the one to its left.)
fn unfilter(tag: u8, row: &mut [u8], previous: &[u8], bpp: usize) -> Result<()> {
    let cells = Cell::from_mut(row).as_slice_of_cells();
    let add = |x: &Cell<u8>, n: u8| x.set(x.get().wrapping_add(n));
    match tag {
        0 => {}
        1 => {
            for window in cells.windows(bpp + 1) {
                if let [left, .., x] = window {
                    add(x, left.get());
                }
            }
        }
        2 => {
            for (x, &up) in cells.iter().zip(previous) {
                add(x, up);
            }
        }
        3 | 4 => {
            // The first pixel has nothing to its left (and the Paeth predictor of it is the byte above).
            for (x, &up) in cells.iter().zip(previous).take(bpp) {
                add(x, if tag == 3 { up / 2 } else { up });
            }
            for (window, above) in cells.windows(bpp + 1).zip(previous.windows(bpp + 1)) {
                if let ([left, .., x], [up_left, .., up]) = (window, above) {
                    let left = left.get();
                    add(x, if tag == 3 { ((u16::from(left) + u16::from(*up)) / 2) as u8 } else { paeth(left, *up, *up_left) });
                }
            }
        }
        _ => return Err(bad_png("a row has a filter that is not one of the five")),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
