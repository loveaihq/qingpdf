//! PNG files written (RFC 2083) for the pictures of a Word file (5b), and the CRC-32 that PNG and ZIP share. Here and not in
//! `qingpdf-core` because the filtering of a page-sized picture is a loop that is worth being built for speed.

/// CRC-32 (the polynomial 0xEDB88320 of ZIP, PNG and gzip), continued: start from 0, feed the pieces, the result is the CRC.
#[allow(clippy::indexing_slicing)] // (the table is made at compile time: a mistake would not build)
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
    let mut c = !crc;
    for &b in data {
        c = TABLE.get(usize::from((c ^ u32::from(b)) as u8)).copied().unwrap_or(0) ^ (c >> 8);
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32_update(crc32_update(0, kind), data).to_be_bytes());
}

/// A PNG of `width` by `height` pixels with `channels` (1 grey, 3 RGB, 4 RGBA) bytes a pixel, rows without padding. Grey with
/// only black and white in it is written with 1 bit a pixel. Otherwise each row takes the filter (none, sub, up, Paeth) with the
/// smallest sum of absolute values, which is the usual heuristic.
pub fn png(width: u32, height: u32, channels: u8, data: &[u8]) -> Vec<u8> {
    let bpp = usize::from(channels.max(1));
    let stride = (width as usize * bpp).max(1);
    let bilevel = channels == 1 && data.iter().all(|&b| b == 0 || b == 255);
    let mut raw: Vec<u8>;
    if bilevel {
        let packed = (width as usize).div_ceil(8);
        raw = Vec::with_capacity((packed + 1) * height as usize);
        for row in data.chunks_exact(stride) {
            raw.push(0);
            for bits in row.chunks(8) {
                raw.push(bits.iter().enumerate().fold(0u8, |acc, (i, &b)| acc | (u8::from(b == 255) << (7 - i))));
            }
        }
    } else {
        raw = Vec::with_capacity((stride + 1) * height as usize);
        let mut cands: [Vec<u8>; 4] = std::array::from_fn(|_| vec![0u8; stride]);
        let zero = vec![0u8; stride];
        for (y, row) in data.chunks_exact(stride).enumerate() {
            let prev = if y == 0 { &zero[..] } else { data.get((y - 1) * stride..y * stride).unwrap_or(&zero) };
            for i in 0..stride {
                let a = if i >= bpp { row.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                let b = prev.get(i).copied().unwrap_or(0);
                let c = if i >= bpp { prev.get(i - bpp).copied().unwrap_or(0) } else { 0 };
                let x = row.get(i).copied().unwrap_or(0);
                let (pa, pb, pc) = ((i32::from(b) - i32::from(c)).abs(), (i32::from(a) - i32::from(c)).abs(), (i32::from(a) + i32::from(b) - 2 * i32::from(c)).abs());
                let paeth = if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                };
                let out = [x, x.wrapping_sub(a), x.wrapping_sub(b), x.wrapping_sub(paeth)];
                for (cand, v) in cands.iter_mut().zip(out) {
                    if let Some(slot) = cand.get_mut(i) {
                        *slot = v;
                    }
                }
            }
            let cost = |v: &Vec<u8>| v.iter().map(|&b| u64::from(if b < 128 { b } else { 255 - b + 1 })).sum::<u64>();
            let best = (0..4).min_by_key(|&k| cands.get(k).map_or(u64::MAX, cost)).unwrap_or(0);
            // (The candidates are filters 0, 1, 2 and 4: the byte for Paeth is 4; 3 is the average, which is not tried.)
            raw.push(if best == 3 { 4 } else { best as u8 });
            raw.extend_from_slice(cands.get(best).map_or(&[][..], |v| v));
        }
    }
    let (depth, color) = match (bilevel, channels) {
        (true, _) => (1, 0),
        (_, 1) => (8, 0),
        (_, 3) => (8, 2),
        _ => (8, 6),
    };
    assemble(width, height, depth, color, &raw, 6)
}

/// A page as a PNG of RGB, written to `sink` a band of rows at a time: the filter Sub on every row and the quickest level of the
/// compression, which is what a page of some megabytes needs (the pictures of a Word file are worth the time to find the filters,
/// this is not). Only the compressor and the rows of one band are held, whatever the size of the page, so a page of hundreds of
/// megabytes can be drawn in pieces and written as they come; the compressed data goes out as it is made, in `IDAT` chunks.
pub struct PageWriter<W: std::io::Write> {
    sink: W,
    deflate: Box<miniz_oxide::deflate::core::CompressorOxide>,
    width: usize,
    rows_left: u32,
    out: Vec<u8>,
}

/// The size of the pieces the compressed data is cut into (the data of one `IDAT` chunk).
const IDAT_BYTES: usize = 1 << 16;

impl<W: std::io::Write> PageWriter<W> {
    /// Start a PNG of `width` by `height` pixels: the signature and the header are written now.
    pub fn new(mut sink: W, width: u32, height: u32) -> std::io::Result<PageWriter<W>> {
        let mut head = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut head, b"IHDR", &ihdr);
        sink.write_all(&head)?;
        let level = miniz_oxide::deflate::CompressionLevel::BestSpeed;
        let deflate = Box::new(miniz_oxide::deflate::core::CompressorOxide::with_format_and_level(miniz_oxide::DataFormat::Zlib, level));
        Ok(PageWriter { sink, deflate, width: width as usize, rows_left: height, out: vec![0; IDAT_BYTES] })
    }

    /// Push `data` into the compressor (`last`: it is all there is), writing what comes out.
    fn push(&mut self, mut data: &[u8], last: bool) -> std::io::Result<()> {
        use miniz_oxide::deflate::core::{TDEFLFlush, TDEFLStatus, compress};
        let flush = if last { TDEFLFlush::Finish } else { TDEFLFlush::None };
        let mut stuck = 0u8;
        loop {
            let (status, used, made) = compress(&mut self.deflate, data, &mut self.out, flush);
            data = data.get(used..).unwrap_or(&[]);
            if made > 0 {
                let mut piece = Vec::with_capacity(made + 12);
                chunk(&mut piece, b"IDAT", self.out.get(..made).unwrap_or(&[]));
                self.sink.write_all(&piece)?;
            }
            match status {
                TDEFLStatus::Done => return Ok(()),
                // (All given and the buffer not full: nothing more is waiting to come out, until the next rows or the end.)
                TDEFLStatus::Okay if !last && data.is_empty() && made < self.out.len() => return Ok(()),
                TDEFLStatus::Okay => {}
                _ => return Err(std::io::Error::other("the compressor failed")),
            }
            stuck = if used == 0 && made == 0 { stuck + 1 } else { 0 };
            if stuck > 2 {
                return Err(std::io::Error::other("the compressor made no progress"));
            }
        }
    }

    /// The next rows of the page (`rgba`: 4 bytes a pixel, the fourth is not written; whole rows).
    pub fn rows(&mut self, rgba: &[u8]) -> std::io::Result<()> {
        let stride = (self.width * 4).max(1);
        let mut raw = Vec::with_capacity((self.width * 3 + 1) * 64);
        for band in rgba.chunks(stride * 64) {
            raw.clear();
            for row in band.chunks_exact(stride) {
                if self.rows_left == 0 {
                    break;
                }
                self.rows_left -= 1;
                raw.push(1);
                let mut left = [0u8; 3];
                for &[r, g, b, _] in row.as_chunks::<4>().0 {
                    let here = [r, g, b];
                    raw.extend(here.iter().zip(&left).map(|(&v, &l)| v.wrapping_sub(l)));
                    left = here;
                }
            }
            self.push(&raw, false)?;
        }
        Ok(())
    }

    /// The end of the data and of the file; the sink is given back. Fewer rows than the header says were given: an error.
    pub fn finish(mut self) -> std::io::Result<W> {
        if self.rows_left != 0 {
            return Err(std::io::Error::other("the page was not all given"));
        }
        self.push(&[], true)?;
        let mut end = Vec::with_capacity(12);
        chunk(&mut end, b"IEND", &[]);
        self.sink.write_all(&end)?;
        Ok(self.sink)
    }
}

/// The page `qingpdf render` draws (`rgba`: 4 bytes a pixel, the fourth is not written) as a PNG of RGB.
pub(super) fn page(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    // (Into a vector nothing fails, but a bitmap that has fewer rows than it says, which a bitmap never has.)
    PageWriter::new(Vec::new(), width, height).and_then(|mut w| w.rows(rgba).and_then(|()| w.finish())).unwrap_or_default()
}

/// The signature, the header, the data (`raw`: the rows with their filter bytes, compressed with zlib at `level`) and the end.
fn assemble(width: u32, height: u32, depth: u8, color: u8, raw: &[u8], level: u8) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(raw, level));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_the_check_string() {
        assert_eq!(crc32_update(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_update(crc32_update(0, b"1234"), b"56789"), 0xCBF4_3926);
    }

    fn decode(bytes: &[u8]) -> (u32, u32, ::png::ColorType, ::png::BitDepth, Vec<u8>) {
        let mut reader = ::png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info.width, info.height, info.color_type, info.bit_depth, buf)
    }

    #[test]
    fn png_round_trips() {
        let rgba: Vec<u8> = (0..(7 * 5 * 4)).map(|i| ((i * 37) % 251) as u8).collect();
        let (w, h, ct, bd, back) = decode(&png(7, 5, 4, &rgba));
        assert_eq!((w, h, ct, bd), (7, 5, ::png::ColorType::Rgba, ::png::BitDepth::Eight));
        assert_eq!(back, rgba);
        let rgb: Vec<u8> = (0..(9 * 3 * 3)).map(|i| (i * 11 % 256) as u8).collect();
        assert_eq!(decode(&png(9, 3, 3, &rgb)).4, rgb);
        // A smooth picture, where the Paeth filter wins on most rows.
        let smooth: Vec<u8> = (0..64 * 64 * 3).map(|i| ((i / 3 % 64) * 3 + (i / 3 / 64) * 2 + i % 3) as u8).collect();
        assert_eq!(decode(&png(64, 64, 3, &smooth)).4, smooth);
        let grey: Vec<u8> = (0..(5 * 4)).map(|i| (i * 13 % 200 + 1) as u8).collect();
        let (_, _, ct, bd, back) = decode(&png(5, 4, 1, &grey));
        assert_eq!((ct, bd), (::png::ColorType::Grayscale, ::png::BitDepth::Eight));
        assert_eq!(back, grey);
    }

    #[test]
    fn a_page_is_rgb_without_the_fourth_byte() {
        for (w, h) in [(7usize, 5usize), (1, 1), (300, 40), (1, 90)] {
            let rgba: Vec<u8> = (0..w * h * 4).map(|i| ((i * 37 + i / 11) % 251) as u8).collect();
            let (width, height, ct, bd, back) = decode(&page(w as u32, h as u32, &rgba));
            assert_eq!((width as usize, height as usize, ct, bd), (w, h, ::png::ColorType::Rgb, ::png::BitDepth::Eight));
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| p.iter().take(3).copied()).collect();
            assert_eq!(back, rgb, "{w} x {h}");
        }
    }

    #[test]
    fn a_page_written_in_bands_is_the_page_written_at_once() {
        let (w, h) = (37usize, 211usize);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| ((i * 7 + i / 13 + (i / 5) % 3) % 251) as u8).collect();
        let whole = page(w as u32, h as u32, &rgba);
        // Bands of 1, 70 and 140 rows (the compressor takes them as they come); the picture is the same, and the data is cut into pieces.
        let mut writer = PageWriter::new(Vec::new(), w as u32, h as u32).unwrap();
        for band in [1usize, 70, 140] {
            let from: usize = [0usize, 1, 71].iter().zip([1usize, 70, 140]).find(|(_, b)| *b == band).map(|(f, _)| *f).unwrap();
            writer.rows(&rgba[from * w * 4..(from + band) * w * 4]).unwrap();
        }
        let banded = writer.finish().unwrap();
        let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| p.iter().take(3).copied()).collect();
        assert_eq!(decode(&whole).4, rgb);
        assert_eq!(decode(&banded).4, rgb);
        // A page that is not all given is an error, and so is one given with too many rows (they are left out).
        let mut short = PageWriter::new(Vec::new(), 2, 3).unwrap();
        short.rows(&[0; 8]).unwrap();
        assert!(short.finish().is_err());
        // A large noisy page comes out in more than one chunk of data.
        let noise: Vec<u8> = (0..300 * 300 * 4u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let big = page(300, 300, &noise);
        assert!(big.windows(4).filter(|w| w == b"IDAT").count() > 1);
        assert_eq!(decode(&big).4.len(), 300 * 300 * 3);
    }

    #[test]
    fn black_and_white_is_one_bit() {
        let bw: Vec<u8> = (0..(13 * 3)).map(|i| if i % 3 == 0 { 255 } else { 0 }).collect();
        let (w, h, ct, bd, back) = decode(&png(13, 3, 1, &bw));
        assert_eq!((w, h, ct, bd), (13, 3, ::png::ColorType::Grayscale, ::png::BitDepth::One));
        // 1 bit a pixel, rows padded to bytes; the first row is 255 at 0, 3, 6, 9, 12 (the decoder gives the bits as they are)
        assert_eq!(back.len(), 2 * 3);
        assert_eq!(back.first().copied(), Some(0b1001_0010));
    }
}
