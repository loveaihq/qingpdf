//! A picture of a file made smaller: decoded to 8-bit samples (a JPEG, a stream of raw samples through its filters, or a JPEG 2000 file),
//! averaged down to the size the page shows it at, and coded again as a baseline JPEG ([`super::jpeg`]). What is made is kept only when it is
//! smaller than what was there. A soft mask goes down to the same size, Flate-coded. Runs on a worker thread of its own: nothing here
//! touches a [`crate::document::Document`] (the caller read the stream, and settled that it is a picture that may be changed).

use miniz_oxide::deflate::compress_to_vec_zlib;
use super::jpeg::{self, Raster};
use crate::error::{Error, Result};
use crate::filter::{self, DecodeBudget};
use crate::object::{Object, Stream};
use crate::render::jpx;
use crate::render::work::{Work, cost};
use crate::render;
use crate::writer::BEST_FLATE_LEVEL;

/// How the samples of a picture are had from its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// `DCTDecode`: the filters in front of it (none, usually) have been undone by the caller.
    Jpeg,
    /// No picture codec: the filters of the dictionary give 8-bit samples, one row after the other.
    Raw,
    /// `JPXDecode`.
    Jpx,
}

/// A picture to make smaller.
pub struct Job {
    /// The picture's dictionary (its `/Filter` and `/DecodeParms` direct objects) and data as they are in the file. For a JPEG, the
    /// data is the JPEG itself.
    pub stream: Stream,
    pub codec: Codec,
    /// `/Width` and `/Height`.
    pub size: (usize, usize),
    /// Colour components: 1 or 3 (0 for a JPEG 2000 file with no colour space in the dictionary: what the file has).
    pub comps: usize,
    /// The size it is to have, no bigger than `size` in either direction.
    pub target: (usize, usize),
    pub quality: u8,
    /// The soft mask: the number of its object, and its dictionary and data (8-bit grey, no `/Decode`, no `/Matte`, no codec but Flate).
    pub mask: Option<(u32, Stream)>,
}

/// What came of a [`Job`].
#[derive(Default)]
pub struct Done {
    /// The new picture, if the JPEG is smaller than the picture was.
    pub image: Option<Stream>,
    /// The new soft mask, with its object number, if the picture was changed and the mask is smaller at its new size.
    pub mask: Option<(u32, Stream)>,
}

/// Is this JPEG coded in more than one scan over the whole picture (a start of frame of a progressive kind)? Looks at the segments up to
/// the frame header only.
fn is_progressive(jpeg: &[u8]) -> bool {
    let mut at = 2;
    while let (Some(&0xFF), Some(&marker)) = (jpeg.get(at), jpeg.get(at + 1)) {
        match marker {
            0xFF => at += 1,
            0x01 | 0xD0..=0xD8 => at += 2,
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => return matches!(marker, 0xC2 | 0xC6 | 0xCA | 0xCE),
            0xD9 | 0xDA => return false,
            _ => match jpeg.get(at + 2..at + 4) {
                Some(&[hi, lo]) => at += 2 + usize::from(u16::from_be_bytes([hi, lo])).max(2),
                _ => return false,
            },
        }
    }
    false
}

/// About how many bytes the work on `job` holds at once (the decoded samples, the smaller ones, the blocks of the encoder).
pub fn memory(job: &Job) -> u64 {
    let (w, h) = (job.size.0 as u64, job.size.1 as u64);
    let (tw, th) = (job.target.0 as u64, job.target.1 as u64);
    let c = job.comps as u64;
    let side = |s: &Stream, key: &str| s.dict.get_int(key).and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
    // The mask is decoded at its own size (what its dictionary says), not the picture's; a progressive JPEG keeps its coefficients (two
    // bytes a sample) besides the samples, a baseline one decodes a few rows at a time.
    let mask = job.mask.as_ref().map_or(0, |(_, m)| side(m, "Width").saturating_mul(side(m, "Height")));
    let coefficients = if job.codec == Codec::Jpeg && is_progressive(&job.stream.data) { w * h * c * 2 } else { 0 };
    (w * h * c).saturating_add(coefficients).saturating_add(mask).saturating_add(tw * th * (c + 3)).saturating_add(job.stream.data.len() as u64)
}

/// How many pixels the picture and the soft mask of a job may have (a bigger picture is left as it is).
pub const MAX_SOURCE_PIXELS: usize = 100 << 20;

/// 8-bit samples, interleaved, `comps` to a pixel.
struct Samples {
    w: usize,
    h: usize,
    comps: usize,
    data: Vec<u8>,
}

fn spend(work: &Work, units: f64) -> Result<()> {
    if work.charge(units) {
        Ok(())
    } else if work.was_cancelled() {
        Err(Error::Cancelled)
    } else {
        Err(Error::Limit("compressing the pictures asks for more work than is allowed".to_string()))
    }
}

fn unsuitable(why: &str) -> Error {
    Error::Unsupported(format!("picture left as it is: {why}"))
}

fn decode(codec: Codec, stream: &Stream, (w, h): (usize, usize), comps: usize, want: (usize, usize), work: &Work) -> Result<Samples> {
    if w == 0 || h == 0 || w.saturating_mul(h) > MAX_SOURCE_PIXELS {
        return Err(unsuitable("its size"));
    }
    match codec {
        Codec::Jpeg => {
            // (What the JPEG's header says is what is allocated: no more than the dictionary's size, which the memory was counted by.)
            let (jw, jh, data) = render::image::jpeg_samples(&stream.data, comps, w * h)?;
            if (jw, jh) != (w, h) {
                return Err(unsuitable("the JPEG is not the size the picture says"));
            }
            spend(work, (jw * jh * comps) as f64 * cost::SQZ_DECODE_SAMPLE)?;
            Ok(Samples { w: jw, h: jh, comps, data })
        }
        Codec::Raw => {
            spend(work, (w * h * comps) as f64 * cost::SQZ_DECODE_SAMPLE)?;
            let budget = DecodeBudget::default();
            let need = w * h * comps;
            let mut data = filter::decode_with_limit(&stream.dict, &stream.data, &|o| Ok(o.clone()), need.saturating_add(1 << 16), need, &budget)?;
            // A picture whose data runs out before its last row is not one to change; a rest after it is ignored.
            if data.len() < need {
                return Err(unsuitable("its data is shorter than its size says"));
            }
            data.truncate(need);
            Ok(Samples { w, h, comps, data })
        }
        Codec::Jpx => {
            // (A picture with no colour space of its own, `comps` 0, is grey or RGB as the file says.)
            let opts = jpx::Options { want, components: (comps != 0).then_some(comps), indexed: false, alpha: jpx::Alpha::Ignore, threads: Some(1) };
            let img = jpx::decode(&stream.data, &opts, work)?;
            if !matches!(img.ncomp, 1 | 3) || (comps != 0 && img.ncomp != comps) || img.alpha.is_some() {
                return Err(unsuitable("the JPEG 2000 file has other channels than the colour space"));
            }
            Ok(Samples { w: img.w, h: img.h, comps: img.ncomp, data: img.data })
        }
    }
}

/// The samples averaged down to `dw` by `dh` (not bigger than they are, not less than 1 pixel each way).
fn average(s: &Samples, dw: usize, dh: usize, work: &Work) -> Result<Samples> {
    let (dw, dh) = (dw.clamp(1, s.w), dh.clamp(1, s.h));
    if (dw, dh) == (s.w, s.h) {
        return Ok(Samples { w: s.w, h: s.h, comps: s.comps, data: s.data.clone() });
    }
    spend(work, (s.w * s.h * s.comps) as f64 * cost::SQZ_AVERAGE_SAMPLE)?;
    Ok(Samples { w: dw, h: dh, comps: s.comps, data: super::average(&s.data, (s.w, s.h), s.comps, (dw, dh)) })
}

/// The picture of `job` as a JPEG, if that is smaller than it is; and its soft mask at the new size.
pub fn shrink(job: Job, work: &Work) -> Result<Done> {
    let Job { stream, codec, size, comps, target, quality, mask } = job;
    let samples = decode(codec, &stream, size, comps, target, work)?;
    let small = average(&samples, target.0.min(samples.w), target.1.min(samples.h), work)?;
    drop(samples);
    spend(work, (small.w * small.h) as f64 * cost::SQZ_ENCODE_PIXEL)?;
    let comps = small.comps;
    let jpeg = jpeg::encode(&Raster { w: small.w, h: small.h, comps, data: &small.data }, quality)?;
    drop(small.data);
    // Smaller by a good part, or not worth the loss: a picture that is a JPEG of the same size has been through enough coding.
    if (jpeg.len() as u64).saturating_mul(20) >= (stream.data.len() as u64).saturating_mul(19) {
        return Ok(Done::default());
    }
    let mut dict = stream.dict;
    dict.remove("DecodeParms");
    dict.remove("Length");
    dict.set("Width", Object::Integer(small.w as i64));
    dict.set("Height", Object::Integer(small.h as i64));
    dict.set("BitsPerComponent", Object::Integer(8));
    dict.set("Filter", Object::from("DCTDecode"));
    if !dict.contains_key("ColorSpace") {
        dict.set("ColorSpace", Object::from(if comps == 1 { "DeviceGray" } else { "DeviceRGB" }));
    }
    let image = Stream { dict, data: jpeg };
    // The soft mask goes down to the same size: the same pixels, a different codec.
    let mut new_mask = None;
    if let Some((num, m)) = mask {
        let msize = (
            m.dict.get("Width").and_then(Object::as_int).and_then(|v| usize::try_from(v).ok()).unwrap_or(0),
            m.dict.get("Height").and_then(Object::as_int).and_then(|v| usize::try_from(v).ok()).unwrap_or(0),
        );
        let planned = decode(Codec::Raw, &m, msize, 1, msize, work).and_then(|s| average(&s, small.w, small.h, work));
        if let Ok(g) = planned {
            spend(work, g.data.len() as f64 * cost::SQZ_FLATE_BYTE)?;
            let data = compress_to_vec_zlib(&g.data, BEST_FLATE_LEVEL);
            if data.len() < m.data.len() {
                let mut dict = m.dict;
                dict.remove("DecodeParms");
                dict.remove("Length");
                dict.set("Width", Object::Integer(g.w as i64));
                dict.set("Height", Object::Integer(g.h as i64));
                dict.set("BitsPerComponent", Object::Integer(8));
                dict.set("Filter", Object::from("FlateDecode"));
                new_mask = Some((num, Stream { dict, data }));
            }
        }
    }
    Ok(Done { image: Some(image), mask: new_mask })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Dict;

    fn raw_job(w: usize, h: usize, comps: usize, target: (usize, usize)) -> Job {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..comps {
                    let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
                    let v = 120.0 + 80.0 * (fx * 5.0 + c as f64).sin() * (fy * 3.0).cos() + ((x * 7 + y * 13 + c) % 11) as f64;
                    data.push(v as u8);
                }
            }
        }
        let mut dict = Dict::new();
        dict.set("Type", Object::from("XObject"));
        dict.set("Subtype", Object::from("Image"));
        dict.set("Width", Object::Integer(w as i64));
        dict.set("Height", Object::Integer(h as i64));
        dict.set("ColorSpace", Object::from(if comps == 1 { "DeviceGray" } else { "DeviceRGB" }));
        dict.set("BitsPerComponent", Object::Integer(8));
        dict.set("Filter", Object::from("FlateDecode"));
        let packed = compress_to_vec_zlib(&data, 6);
        Job { stream: Stream { dict, data: packed }, codec: Codec::Raw, size: (w, h), comps, target, quality: 75, mask: None }
    }

    #[test]
    fn averaging_keeps_the_mean_and_the_shape() {
        let work = Work::new();
        // A flat picture stays flat whatever the sizes; the average of 2 by 2 blocks is the mean of the four.
        let flat = Samples { w: 7, h: 5, comps: 3, data: vec![90; 7 * 5 * 3] };
        for (dw, dh) in [(3, 2), (1, 1), (7, 5), (4, 5), (6, 1)] {
            let s = average(&flat, dw, dh, &work).unwrap();
            assert_eq!((s.w, s.h, s.data.len()), (dw, dh, dw * dh * 3));
            assert!(s.data.iter().all(|&v| v == 90), "{dw} by {dh}");
        }
        let src = Samples { w: 4, h: 2, comps: 1, data: vec![0, 10, 100, 110, 20, 30, 120, 130] };
        let s = average(&src, 2, 1, &work).unwrap();
        assert_eq!(s.data, [15, 115]);
        // Not bigger than the picture is, never smaller than one pixel.
        let s = average(&src, 40, 0, &work).unwrap();
        assert_eq!((s.w, s.h), (4, 1));
    }

    #[test]
    fn a_raw_picture_becomes_a_smaller_jpeg_with_the_dictionary_to_match() {
        let job = raw_job(400, 300, 3, (200, 150));
        let before = job.stream.data.len();
        let done = shrink(job, &Work::new()).unwrap();
        let image = done.image.expect("a smaller picture");
        assert!(image.data.len() < before);
        assert_eq!(image.dict.get_int("Width"), Some(200));
        assert_eq!(image.dict.get_int("Height"), Some(150));
        assert_eq!(image.dict.get_name("Filter").unwrap(), &crate::object::Name::from("DCTDecode"));
        assert!(!image.dict.contains_key("DecodeParms") && image.dict.get_name("ColorSpace").is_some());
        assert!(image.data.starts_with(&[0xFF, 0xD8]));
    }

    #[test]
    fn a_picture_that_would_not_get_smaller_is_left_alone_and_a_mask_follows_the_picture() {
        // Noise: a JPEG of it is not 5% smaller than its Flate data at a size that is the same. (The generator is fixed.)
        let (w, h) = (64, 64);
        let mut seed = 7u32;
        let data: Vec<u8> = (0..w * h).map(|_| { seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (seed >> 24) as u8 }).collect();
        let mut job = raw_job(w, h, 1, (w, h));
        job.stream.data = compress_to_vec_zlib(&data, 6);
        job.quality = 100;
        let before = job.stream.data.len();
        let kept = shrink(job, &Work::new()).unwrap();
        assert!(kept.image.is_none() && kept.mask.is_none(), "{} of {before}", kept.image.map_or(0, |i| i.data.len()));
        // With a mask: it comes out at the new size, Flate-coded.
        let mut job = raw_job(300, 200, 3, (100, 100));
        let mask = raw_job(300, 200, 1, (300, 200)).stream;
        job.mask = Some((9, mask));
        let done = shrink(job, &Work::new()).unwrap();
        let (num, m) = done.mask.expect("the mask follows");
        assert_eq!((num, m.dict.get_int("Width"), m.dict.get_int("Height")), (9, Some(100), Some(100)));
        assert_eq!(m.dict.get_name("Filter").unwrap(), &crate::object::Name::from("FlateDecode"));
    }

    #[test]
    fn a_progressive_jpeg_is_told_from_a_baseline_one() {
        let samples: Vec<u8> = (0..64 * 64 * 3).map(|i| (i % 251) as u8).collect();
        let baseline = jpeg::encode(&Raster { w: 64, h: 64, comps: 3, data: &samples }, 75).unwrap();
        assert!(!is_progressive(&baseline));
        // The same file with its frame marker changed to a progressive one (the test is about the marker, not the data).
        let mut progressive = baseline.clone();
        let sof = progressive.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        progressive[sof + 1] = 0xC2;
        assert!(is_progressive(&progressive));
        assert!(!is_progressive(&[]) && !is_progressive(&[0xFF, 0xD8]) && !is_progressive(&[0xFF, 0xD8, 0xFF]) && !is_progressive(&[1, 2, 3, 4, 5, 6]));
        let mut job = raw_job(300, 200, 3, (100, 100));
        job.codec = Codec::Jpeg;
        job.stream.data = progressive;
        let with = memory(&job);
        job.stream.data = baseline;
        assert!(with >= memory(&job) + 300 * 200 * 3 * 2 - 1000, "{with} against {}", memory(&job));
    }

    #[test]
    fn the_memory_counts_the_mask_at_its_own_size() {
        let plain = raw_job(300, 200, 3, (100, 100));
        let without = memory(&plain);
        // A mask of 3000 by 2000 (its dictionary says) is decoded at that size, whatever the picture's size.
        let mut job = raw_job(300, 200, 3, (100, 100));
        job.mask = Some((9, raw_job(3000, 2000, 1, (3000, 2000)).stream));
        assert!(memory(&job) >= without + 6_000_000, "{} against {without}", memory(&job));
        // A mask whose dictionary says nothing counts for nothing, and does not overflow.
        let mut job = raw_job(300, 200, 3, (100, 100));
        let mut bare = raw_job(300, 200, 1, (300, 200)).stream;
        bare.dict.set("Width", Object::Integer(i64::MAX));
        bare.dict.set("Height", Object::Integer(i64::MAX));
        job.mask = Some((9, bare));
        assert!(memory(&job) > 1 << 40);
    }

    #[test]
    fn what_is_not_what_the_dictionary_says_is_an_error_not_a_panic() {
        // Data shorter than the size says.
        let mut job = raw_job(50, 50, 3, (25, 25));
        job.stream.data = compress_to_vec_zlib(&[1, 2, 3], 6);
        assert!(shrink(job, &Work::new()).is_err());
        // Not a JPEG, and a JPEG of the wrong number of components.
        let mut job = raw_job(8, 8, 3, (4, 4));
        job.codec = Codec::Jpeg;
        job.stream.data = vec![0xFF, 0xD8, 1, 2, 3];
        assert!(shrink(job, &Work::new()).is_err());
        let grey = jpeg::encode(&Raster { w: 16, h: 16, comps: 1, data: &[100; 256] }, 75).unwrap();
        let mut job = raw_job(16, 16, 3, (8, 8));
        job.codec = Codec::Jpeg;
        job.stream.data = grey;
        assert!(shrink(job, &Work::new()).is_err());
        // A JPEG that is not the size the dictionary says, bigger or smaller: left as it is (and the big one is not decoded).
        for (jw, jh) in [(64usize, 64usize), (8, 8)] {
            let samples: Vec<u8> = (0..jw * jh * 3).map(|i| (i % 251) as u8).collect();
            let wrong = jpeg::encode(&Raster { w: jw, h: jh, comps: 3, data: &samples }, 75).unwrap();
            let mut job = raw_job(16, 16, 3, (8, 8));
            job.codec = Codec::Jpeg;
            job.stream.data = wrong;
            let e = shrink(job, &Work::new()).err().expect("a JPEG of another size");
            assert!(matches!(e, Error::Unsupported(_)), "{jw}: {e}");
        }
        // Nonsense sizes.
        let job = raw_job(0, 0, 1, (0, 0));
        assert!(shrink(job, &Work::new()).is_err());
        // No work left.
        let job = raw_job(100, 100, 3, (50, 50));
        assert!(matches!(shrink(job, &Work::with_allowance(10.0)), Err(Error::Limit(_))));
    }

    #[test]
    fn a_jpeg_is_decoded_and_made_again_smaller() {
        let (w, h) = (320, 240);
        let src: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) * 255 / w + (i / 3 / w) % 17 * 4 + i % 3 * 20) as u8).collect();
        let original = jpeg::encode(&Raster { w, h, comps: 3, data: &src }, 95).unwrap();
        let mut job = raw_job(w, h, 3, (160, 120));
        job.codec = Codec::Jpeg;
        job.stream.data = original.clone();
        job.stream.dict.set("Filter", Object::from("DCTDecode"));
        let done = shrink(job, &Work::new()).unwrap();
        let image = done.image.unwrap();
        assert!(image.data.len() < original.len() / 2);
        assert_eq!((image.dict.get_int("Width"), image.dict.get_int("Height")), (Some(160), Some(120)));
    }
}
