//! Images (ISO 32000-1 8.9): the samples of an image XObject or inline image turned
//! into pixels, shrunk by a whole factor when the page shows them smaller than they are
//! (a 300 dpi scan on a 150 dpi page is read once and drawn once, not resampled by the
//! rasterizer). Filters: those of [`crate::filter`], `DCTDecode` (zune-jpeg) and
//! `CCITTFaxDecode` ([`super::ccitt`]), `JBIG2Decode` ([`super::jbig2`]), `JPXDecode` ([`super::jpx`], decoded
//! only down to the size the page shows). What cannot be decoded comes back as [`Loaded::Placeholder`].

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use tiny_skia::Pixmap;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace as JpegSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, Name, Object, Stream};
use crate::text::font::Warnings;

use super::ccitt;
use super::color::ColorSpace;
use super::func::Meter;
use super::jbig2;
use super::jpx;
use super::work::{Held, Work};

/// Most pixels one image may have.
pub(crate) const MAX_IMAGE_PIXELS: u64 = 128 * 1024 * 1024;
/// Most bytes of decoded samples one image may have (raw, before conversion).
const MAX_IMAGE_BYTES: usize = 48 * 1024 * 1024;
/// Most pixels of the picture an image is turned into (4 bytes each): a picture shown bigger than this
/// is drawn from a smaller copy.
const MAX_OUTPUT_PIXELS: usize = 12 * 1024 * 1024;
/// Most pixels a soft or explicit mask is shrunk to before it is spread over the picture (1 byte each, 4 while
/// it is shrunk); beyond this it is sampled from the smaller copy.
const MAX_MASK_PIXELS: usize = 4 * 1024 * 1024;
/// Widest a CCITT image may be (real faxes are a few thousand pixels); the runs of a row are kept in memory.
const MAX_CCITT_COLUMNS: usize = 1 << 20;

pub(crate) enum Loaded {
    /// Premultiplied RGBA, `w` by `h`; the image's whole unit square. `opaque`: no pixel is transparent.
    Image { pixmap: Pixmap, opaque: bool },
    /// An image we cannot decode (a JBIG2 or JPEG 2000 stream with nothing in it): drawn as a grey block.
    Placeholder,
}

pub(crate) struct Env<'a> {
    pub doc: &'a Document,
    /// Finds a name in the resources' `/ColorSpace`.
    pub lookup: &'a dyn Fn(&[u8]) -> Option<Object>,
    /// The fill colour, for stencil masks.
    pub fill: [f32; 3],
    /// About how big the image is on the device, in pixels (width, height): it is shrunk to that, no
    /// smaller, and left as it is when it is smaller already.
    pub target: (usize, usize),
    /// The page's work allowance for tint functions.
    pub meter: &'a Meter,
    /// Source pixels the page may still decode (images, and the masks of images, as they really are).
    pub pixels_left: &'a Cell<u64>,
    /// The page's work meter, for the decoders whose cost tells nothing from the size of the image (JBIG2).
    pub work: &'a Work,
    /// The JBIG2 globals the page has decoded already (the images that share a stream decode it once).
    pub globals: &'a jbig2::GlobalsCache,
}

impl Env<'_> {
    /// Count `n` source pixels against the page. [`Error::Limit`] when it has not got that many left.
    fn charge_pixels(&self, n: u64) -> Result<()> {
        match self.pixels_left.get().checked_sub(n) {
            Some(left) => {
                self.pixels_left.set(left);
                Ok(())
            }
            None => Err(Error::Limit("the page's images have more pixels than is allowed".to_string())),
        }
    }
}

/// The full key names of an inline image's abbreviations (8.9.7, Tables 93 and 94).
pub(crate) fn expand_inline(dict: &Dict) -> Dict {
    let key = |k: &[u8]| -> &'static str {
        match k {
            b"BPC" => "BitsPerComponent",
            b"CS" => "ColorSpace",
            b"D" => "Decode",
            b"DP" => "DecodeParms",
            b"F" => "Filter",
            b"H" => "Height",
            b"IM" => "ImageMask",
            b"I" => "Interpolate",
            b"W" => "Width",
            _ => "",
        }
    };
    let filter_name = |n: &Name| -> Name {
        Name::new(match n.as_bytes() {
            b"AHx" => &b"ASCIIHexDecode"[..],
            b"A85" => b"ASCII85Decode",
            b"LZW" => b"LZWDecode",
            b"Fl" => b"FlateDecode",
            b"RL" => b"RunLengthDecode",
            b"CCF" => b"CCITTFaxDecode",
            b"DCT" => b"DCTDecode",
            other => other,
        })
    };
    let color_name = |n: &Name| -> Name {
        Name::new(match n.as_bytes() {
            b"G" => &b"DeviceGray"[..],
            b"RGB" => b"DeviceRGB",
            b"CMYK" => b"DeviceCMYK",
            b"I" => b"Indexed",
            other => other,
        })
    };
    let mut out = Dict::new();
    for (k, v) in dict.iter() {
        let full = key(k.as_bytes());
        let name = if full.is_empty() { String::from_utf8_lossy(k.as_bytes()).into_owned() } else { full.to_string() };
        let value = match name.as_str() {
            "Filter" => match v {
                Object::Name(n) => Object::Name(filter_name(n)),
                Object::Array(items) => {
                    Object::Array(items.iter().map(|o| if let Object::Name(n) = o { Object::Name(filter_name(n)) } else { o.clone() }).collect())
                }
                other => other.clone(),
            },
            "ColorSpace" => match v {
                Object::Name(n) => Object::Name(color_name(n)),
                Object::Array(items) => {
                    Object::Array(items.iter().enumerate().map(|(i, o)| match o {
                        Object::Name(n) if i < 2 => Object::Name(color_name(n)),
                        other => other.clone(),
                    }).collect())
                }
                other => other.clone(),
            },
            _ => v.clone(),
        };
        out.set(name.as_str(), value);
    }
    out
}

struct Samples {
    w: usize,
    h: usize,
    bpc: u32,
    ncomp: usize,
    stride: usize,
    data: Vec<u8>,
    /// The opacity a JPEG 2000 file has for each pixel (one byte each, `w` by `h`), when the dictionary asks for it.
    alpha: Option<Vec<u8>>,
}

/// The entry `key` of `dict`, read through the page's meter (a reference is read once for the page and kept).
fn get<'d>(dict: &'d Dict, env: &Env<'_>, key: &str) -> Option<Held<'d>> {
    env.work.read(env.doc, dict.get(key)?)
}

fn int(env: &Env<'_>, dict: &Dict, key: &str) -> Option<i64> {
    get(dict, env, key).and_then(|o| o.as_int().or_else(|| o.as_f64().map(|f| f as i64)))
}

fn number_list(env: &Env<'_>, obj: &Object) -> Option<Vec<f64>> {
    let items = obj.as_array()?;
    items.iter().map(|o| env.work.read(env.doc, o).and_then(|o| o.as_f64()).filter(|v| v.is_finite())).collect()
}

const SPECIAL: [&[u8]; 4] = [b"DCTDecode", b"CCITTFaxDecode", b"JBIG2Decode", b"JPXDecode"];

/// Load an image stream.
pub(crate) fn load(env: &Env<'_>, stream: &Stream, warnings: &mut Warnings) -> Result<Loaded> {
    let doc = env.doc;
    let dict = &stream.dict;
    let w = int(env, dict, "Width").unwrap_or(0);
    let h = int(env, dict, "Height").unwrap_or(0);
    if w <= 0 || h <= 0 || (w as u64).saturating_mul(h as u64) > MAX_IMAGE_PIXELS {
        return Err(Error::Limit(format!("image of {w} by {h} pixels is empty or too large")));
    }
    let (w, h) = (w as usize, h as usize);
    let is_mask = matches!(get(dict, env, "ImageMask").as_deref(), Some(Object::Bool(true)));
    let declared_bpc = int(env, dict, "BitsPerComponent");
    // (A JPEG 2000 file has its own bits per sample: the dictionary's entry is ignored, 7.4.9.)
    let is_jpx = filter_list(env, dict).iter().any(|n| n.as_bytes() == b"JPXDecode");
    let bpc = if is_mask { 1 } else if is_jpx { 8 } else { declared_bpc.unwrap_or(8) };
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return Err(Error::Invalid(format!("image with {bpc} bits per component")));
    }
    env.charge_pixels((w as u64).saturating_mul(h as u64))?;
    let space: Option<Arc<ColorSpace>> = if is_mask {
        None
    } else {
        get(dict, env, "ColorSpace").and_then(|o| ColorSpace::load(doc, &o, env.lookup, env.meter))
    };
    let has_mask = !is_mask && ["SMask", "Mask"].iter().any(|k| matches!(get(dict, env, k).as_deref(), Some(Object::Stream(_))));
    // The size the picture is made at: the image's own, shrunk to its place on the device (and to a size we can hold).
    let out_size = |w: usize, h: usize| -> (usize, usize) {
        let (mut out_w, mut out_h) = (w.min(env.target.0).max(1), h.min(env.target.1).max(1));
        if out_w * out_h > MAX_OUTPUT_PIXELS {
            let f = (MAX_OUTPUT_PIXELS as f64 / (out_w * out_h) as f64).sqrt();
            out_w = ((out_w as f64 * f) as usize).max(1);
            out_h = ((out_h as f64 * f) as usize).max(1);
        }
        (out_w, out_h)
    };
    // The mask is made first when the picture's size is known now (not so for a JPEG, which says how big it is itself):
    // its buffers are gone before the picture's are made, so the two are never held together.
    let mut alpha = None;
    let alpha_made = has_mask && !filter_list(env, dict).iter().any(|n| matches!(n.as_bytes(), b"DCTDecode" | b"JPXDecode"));
    if alpha_made {
        let (out_w, out_h) = out_size(w, h);
        alpha = soft_alpha(env, dict, out_w, out_h, warnings)?;
    }
    let indexed = matches!(space.as_deref(), Some(ColorSpace::Indexed { .. }));
    let Some(mut samples) = read_samples(env, stream, w, h, bpc as u32, (space.as_ref().map(|s| s.components()), indexed), warnings)? else {
        return Ok(Loaded::Placeholder);
    };
    // A JPEG (or a JPEG 2000 picture) says how big it is itself; that is what the samples are.
    let (w, h) = (samples.w, samples.h);
    let file_alpha = samples.alpha.take();
    // (A JPEG 2000 picture has its own range of samples: /Decode is ignored, unless the picture is a stencil mask, 7.4.9.)
    let decode = get(dict, env, "Decode").and_then(|o| number_list(env, &o)).filter(|_| is_mask || !is_jpx);
    let space = if is_mask {
        None
    } else {
        match space {
            Some(s) if s.components() == samples.ncomp => Some(s),
            _ => Some(Arc::new(match samples.ncomp {
                1 => ColorSpace::Gray,
                4 => ColorSpace::Cmyk,
                _ => ColorSpace::Rgb,
            })),
        }
    };
    let (out_w, out_h) = out_size(w, h);
    let mut pixels = match &space {
        None => resample(&samples, &stencil_plan(decode.as_deref(), env.fill), None, out_w, out_h),
        Some(space) => {
            let key = color_key(env, dict, &samples);
            let plan = color_plan(&samples, space, decode.as_deref(), key.clone(), env.meter);
            resample(&samples, &plan, key.as_deref(), out_w, out_h)
        }
    };
    drop(samples);
    if has_mask && !alpha_made {
        alpha = soft_alpha(env, dict, out_w, out_h, warnings)?;
    }
    // The opacity channel of a JPEG 2000 file (/SMaskInData), shrunk like the picture; an explicit mask comes first.
    if alpha.is_none()
        && let Some(plane) = file_alpha
    {
        let a = Samples { w, h, bpc: 8, ncomp: 1, stride: w, data: plane, alpha: None };
        let px = resample(&a, &Plan::Gray, None, out_w, out_h);
        alpha = Some(px.chunks_exact(4).map(|p| p.first().copied().unwrap_or(255)).collect());
    }
    if let Some(alpha) = alpha {
        for (px, &a) in pixels.chunks_exact_mut(4).zip(&alpha) {
            if let Some(d) = px.get_mut(3) {
                *d = ((u32::from(*d) * u32::from(a) + 127) / 255) as u8;
            }
        }
    }
    // The resampler leaves straight colour; the pixmap holds it premultiplied (in place: no second copy).
    let mut opaque = true;
    for px in pixels.chunks_exact_mut(4) {
        if let [r, g, b, a] = px
            && *a != 255
        {
            opaque = false;
            let a = u32::from(*a);
            for c in [r, g, b] {
                *c = ((u32::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
    let size = tiny_skia::IntSize::from_wh(out_w as u32, out_h as u32).ok_or_else(|| Error::Limit("image too large".to_string()))?;
    let pixmap = Pixmap::from_vec(pixels, size).ok_or_else(|| Error::Limit("image too large".to_string()))?;
    Ok(Loaded::Image { pixmap, opaque })
}

/// The alpha of the image's `/SMask` or `/Mask` (8.9.6) for `ow` by `oh` pixels; `None`: it has none we can use.
fn soft_alpha(env: &Env<'_>, dict: &Dict, ow: usize, oh: usize, warnings: &mut Warnings) -> Result<Option<Vec<u8>>> {
    if let Some(Object::Stream(mask)) = get(dict, env, "SMask").as_deref() {
        mask_alpha(env, mask, false, ow, oh, warnings)
    } else if let Some(Object::Stream(mask)) = get(dict, env, "Mask").as_deref() {
        mask_alpha(env, mask, true, ow, oh, warnings)
    } else {
        Ok(None)
    }
}

/// `/Mask [min max ...]` (8.9.6.4): ranges of raw sample values that are not painted.
fn color_key(env: &Env<'_>, dict: &Dict, samples: &Samples) -> Option<Vec<(u16, u16)>> {
    let list = number_list(env, &*get(dict, env, "Mask")?)?;
    if list.len() != samples.ncomp * 2 {
        return None;
    }
    Some(list.chunks_exact(2).map(|c| (c.first().copied().unwrap_or(0.0).max(0.0) as u16, c.get(1).copied().unwrap_or(0.0).max(0.0) as u16)).collect())
}

/// The names in a stream's `/Filter`.
fn filter_list(env: &Env<'_>, dict: &Dict) -> Vec<Name> {
    match get(dict, env, "Filter").as_deref() {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(items)) => items.iter().filter_map(|o| env.work.read(env.doc, o).and_then(|o| o.as_name().cloned())).collect(),
        _ => Vec::new(),
    }
}

/// Decode the filters and hand back the raw samples. `None`: an image we cannot decode.
fn read_samples(
    env: &Env<'_>,
    stream: &Stream,
    w: usize,
    h: usize,
    bpc: u32,
    // How many components the dictionary's colour space has, and whether it is an `Indexed` one.
    (space_comps, indexed): (Option<usize>, bool),
    warnings: &mut Warnings,
) -> Result<Option<Samples>> {
    let doc = env.doc;
    let dict = &stream.dict;
    // The filter list, up to the first one that is an image codec.
    let names = filter_list(env, dict);
    let parms: Vec<Option<Dict>> = match get(dict, env, "DecodeParms").as_deref() {
        Some(Object::Dict(d)) => vec![Some(d.clone())],
        Some(Object::Array(items)) => items.iter().map(|o| env.work.read(doc, o).and_then(|o| if let Object::Dict(d) = &*o { Some(d.clone()) } else { None })).collect(),
        _ => Vec::new(),
    };
    let special = names.iter().position(|n| SPECIAL.contains(&n.as_bytes()));
    let (data, codec) = match special {
        None => (doc.decode_stream_limited(stream, MAX_IMAGE_BYTES)?, None),
        Some(i) => {
            let mut inner = stream.clone();
            let before: Vec<Object> = names.iter().take(i).cloned().map(Object::Name).collect();
            if before.is_empty() {
                inner.dict.remove("Filter");
                inner.dict.remove("DecodeParms");
            } else {
                inner.dict.set("Filter", Object::Array(before));
                inner.dict.set("DecodeParms", Object::Array(parms.iter().take(i).map(|p| p.clone().map_or(Object::Null, Object::Dict)).collect()));
            }
            let data = doc.decode_stream_limited(&inner, MAX_IMAGE_BYTES)?;
            if i + 1 != names.len() {
                return Err(Error::Unsupported("a filter after an image codec".to_string()));
            }
            (data, names.get(i).map(|n| (n.clone(), parms.get(i).cloned().flatten())))
        }
    };
    let Some((codec, parm)) = codec else {
        let ncomp = space_comps.unwrap_or(1);
        let stride = (w * ncomp * bpc as usize).div_ceil(8);
        return Ok(Some(Samples { w, h, bpc, ncomp, stride, data, alpha: None }));
    };
    match codec.as_bytes() {
        b"DCTDecode" => jpeg(env, &data, (w as u64).saturating_mul(h as u64)).map(Some),
        b"CCITTFaxDecode" => {
            let p = |key: &str| parm.as_ref().and_then(|d| d.get(key)).and_then(|o| env.work.read(doc, o));
            let flag = |key: &str| matches!(p(key).as_deref(), Some(Object::Bool(true)));
            let columns = p("Columns").and_then(|o| o.as_int()).filter(|&c| c > 0).unwrap_or(1728) as usize;
            if columns > MAX_CCITT_COLUMNS {
                return Err(Error::Limit("CCITT image too wide".to_string()));
            }
            let params = ccitt::Params {
                k: p("K").and_then(|o| o.as_int()).unwrap_or(0),
                columns,
                rows: h,
                byte_align: flag("EncodedByteAlign"),
                black_is_1: flag("BlackIs1"),
            };
            let budget = usize::try_from(doc.budget_left()).unwrap_or(usize::MAX).min(MAX_IMAGE_BYTES);
            let decoded = ccitt::decode(&data, &params, budget);
            doc.charge_decoding(decoded.data.len())?;
            if decoded.damaged {
                warnings.add("a CCITT image is damaged; the rows after the damage are white");
            } else if decoded.rows < h {
                warnings.add(format!("a CCITT image has {} rows of the {h} it declares; the rest are white", decoded.rows));
            }
            // Rows the data did not have are white.
            let stride = columns.div_ceil(8);
            let mut rows = decoded.data;
            let white = if params.black_is_1 { 0 } else { 0xFF };
            let total = stride.checked_mul(h).filter(|&t| t <= MAX_IMAGE_BYTES).ok_or_else(|| Error::Limit("CCITT image too large".to_string()))?;
            rows.resize(total, white);
            // The image's width may differ from /Columns: use the image's.
            let image_stride = w.div_ceil(8);
            let data = if stride == image_stride {
                rows
            } else {
                let mut fixed = vec![white; image_stride * h];
                for (dst, src) in fixed.chunks_exact_mut(image_stride).zip(rows.chunks_exact(stride)) {
                    let n = image_stride.min(stride);
                    if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
                fixed
            };
            Ok(Some(Samples { w, h, bpc: 1, ncomp: 1, stride: image_stride, data, alpha: None }))
        }
        b"JBIG2Decode" => {
            let entry = parm.as_ref().and_then(|d| d.get("JBIG2Globals"));
            // Read only when the page has not decoded this stream already.
            let load = || entry.and_then(|o| env.work.read(doc, o)).and_then(|o| if let Object::Stream(s) = &*o { doc.decode_stream_limited(s, MAX_IMAGE_BYTES).ok() } else { None });
            let globals = entry.map(|o| jbig2::GlobalsSource { key: o.as_obj_ref(), cache: env.globals, load: &load });
            let page = match jbig2::decode_with_globals(&data, globals, (w, h), env.work) {
                Ok(p) => p,
                Err(Error::Limit(m)) => return Err(Error::Limit(m)),
                Err(e) => {
                    warnings.add(format!("a JBIG2 image cannot be decoded ({e}); shown as a grey block"));
                    return Ok(None);
                }
            };
            // The JBIG2 page may be bigger than the dictionary says: what it really has is charged as well.
            env.charge_pixels(page.page_pixels.saturating_sub((w as u64).saturating_mul(h as u64)))?;
            doc.charge_decoding(page.data.len())?;
            if let Some(m) = page.warning {
                warnings.add(format!("a JBIG2 image is damaged ({m}); what could be decoded is drawn"));
            }
            Ok(Some(Samples { w, h, bpc: 1, ncomp: 1, stride: w.div_ceil(8), data: page.data, alpha: None }))
        }
        _ => {
            // JPXDecode.
            let alpha = match int(env, dict, "SMaskInData") {
                Some(1) => jpx::Alpha::Straight,
                Some(2) => jpx::Alpha::Premultiplied,
                _ => jpx::Alpha::Ignore,
            };
            let opts = jpx::Options { want: env.target, components: space_comps, indexed, alpha, threads: None };
            let img = match jpx::decode(&data, &opts, env.work) {
                Ok(img) => img,
                Err(Error::Limit(m)) => return Err(Error::Limit(m)),
                Err(e) => {
                    warnings.add(format!("a JPX image cannot be decoded ({e}); shown as a grey block"));
                    return Ok(None);
                }
            };
            // What the file really has is charged as well, whatever the dictionary says.
            env.charge_pixels(img.full.0.saturating_mul(img.full.1).saturating_sub((w as u64).saturating_mul(h as u64)))?;
            doc.charge_decoding(img.data.len())?;
            if let Some(m) = img.warning {
                warnings.add(format!("a JPX image is damaged ({m}); what could be decoded is drawn"));
            }
            Ok(Some(Samples { w: img.w, h: img.h, bpc: 8, ncomp: img.ncomp, stride: img.w * img.ncomp, data: img.data, alpha: img.alpha }))
        }
    }
}

/// Decode a JPEG. `declared` is the number of pixels the image dictionary says; what the JPEG really has is
/// charged to the page as well when it is more.
fn jpeg(env: &Env<'_>, data: &[u8], declared: u64) -> Result<Samples> {
    let doc = env.doc;
    let bad = |e: &dyn std::fmt::Debug| Error::syntax(None, format!("JPEG: {e:?}"));
    let options = DecoderOptions::default().set_max_width(65_535).set_max_height(65_535);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder.decode_headers().map_err(|e| bad(&e))?;
    let info = decoder.info().ok_or_else(|| Error::syntax(None, "JPEG without a frame header"))?;
    let (jw, jh) = (usize::from(info.width), usize::from(info.height));
    if jw == 0 || jh == 0 || (jw * jh) as u64 > MAX_IMAGE_PIXELS {
        return Err(Error::Limit("JPEG image too large".to_string()));
    }
    env.charge_pixels(((jw * jh) as u64).saturating_sub(declared))?;
    let input = decoder.input_colorspace().unwrap_or(JpegSpace::RGB);
    let (out, ncomp) = match input {
        JpegSpace::Luma => (JpegSpace::Luma, 1),
        JpegSpace::CMYK => (JpegSpace::CMYK, 4),
        // YCCK is taken as it is and turned into CMYK below (the decoder's own conversion to RGB
        // assumes inverted ink values; the PDF says that itself with /Decode when it is so).
        JpegSpace::YCCK => (JpegSpace::YCCK, 4),
        _ => (JpegSpace::RGB, 3),
    };
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options.jpeg_set_out_colorspace(out));
    decoder.decode_headers().map_err(|e| bad(&e))?;
    let size = decoder.output_buffer_size().ok_or_else(|| Error::syntax(None, "JPEG without a frame header"))?;
    if size > MAX_IMAGE_BYTES {
        return Err(Error::Limit("JPEG image too large".to_string()));
    }
    doc.charge_decoding(size)?;
    let mut pixels = vec![0u8; size];
    decoder.decode_into(&mut pixels).map_err(|e| bad(&e))?;
    if input == JpegSpace::YCCK {
        // As libjpeg does (and 8.9.5 leaves to the filter): the YCC part is RGB, the ink is 255 minus it.
        for px in pixels.chunks_exact_mut(4) {
            if let [y, cb, cr, _] = px {
                let (yy, pb, pr) = (f32::from(*y), f32::from(*cb) - 128.0, f32::from(*cr) - 128.0);
                let r = yy + 1.402 * pr;
                let g = yy - 0.344_136 * pb - 0.714_136 * pr;
                let b = yy + 1.772 * pb;
                *y = (255.0 - r).clamp(0.0, 255.0).round() as u8;
                *cb = (255.0 - g).clamp(0.0, 255.0).round() as u8;
                *cr = (255.0 - b).clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    Ok(Samples { w: jw, h: jh, bpc: 8, ncomp, stride: jw * ncomp, data: pixels, alpha: None })
}

/// One row of raw sample values, one byte each (16-bit samples: the high byte).
fn unpack_row<'a>(s: &'a Samples, y: usize, buf: &'a mut Vec<u8>) -> &'a [u8] {
    let n = s.w * s.ncomp;
    let empty: &[u8] = &[];
    let row = s.data.get(y * s.stride..(y + 1) * s.stride).unwrap_or(empty);
    match s.bpc {
        8 if row.len() >= n => row.get(..n).unwrap_or(empty),
        _ => {
            buf.clear();
            match s.bpc {
                8 => buf.extend_from_slice(row),
                16 => buf.extend(row.chunks_exact(2).take(n).map(|c| c.first().copied().unwrap_or(0))),
                b => {
                    let per = (8 / b) as usize;
                    let mask = (1u8 << b) - 1;
                    for &byte in row {
                        for i in 0..per {
                            buf.push((byte >> (8 - b as usize * (i + 1))) & mask);
                        }
                    }
                }
            }
            buf.resize(n, 0);
            buf.get(..n).unwrap_or(empty)
        }
    }
}

// --- shrinking: the average of the source pixels under each pixel of the result --------------------------

/// Where the edges of the `ow` output pixels fall among `sw` source pixels (`ow + 1` edges): the source
/// pixel and how far into it. The average over an output pixel is the difference of the running total of
/// the row at its two edges, over the width between them.
struct Edges {
    index: Vec<usize>,
    frac: Vec<f64>,
    /// The same in 32 bits, for rows short enough to add up in `f32` exactly.
    index32: Vec<u32>,
    frac32: Vec<f32>,
    /// Output pixels per source pixel: the reciprocal of the width between two edges.
    scale: f64,
}

fn edges(sw: usize, ow: usize) -> Edges {
    let mut index = Vec::with_capacity(ow + 1);
    let mut frac = Vec::with_capacity(ow + 1);
    for o in 0..=ow {
        let at = o * sw;
        index.push(at / ow);
        frac.push((at % ow) as f64 / ow as f64);
    }
    let index32 = index.iter().map(|&i| u32::try_from(i).unwrap_or(u32::MAX)).collect();
    let frac32 = frac.iter().map(|&f| f as f32).collect();
    Edges { index, frac, index32, frac32, scale: ow as f64 / sw as f64 }
}

/// Rows of at most this many pixels have totals that `f32` holds exactly (255 times this is below 2^24).
const EXACT_F32_ROW: usize = 65_000;

/// [`shrink_bytes`] for rows short enough for totals in 32 bits: the common case, a good deal faster.
fn shrink_bytes_f32<const N: usize>(row: &[u8], sw: usize, e: &Edges, out: &mut [f32]) {
    let scale = e.scale as f32;
    let mut sum = [0u32; N];
    let mut at = 0usize;
    let mut prev = [0f32; N];
    for (o, (&i, &f)) in e.index32.iter().zip(&e.frac32).enumerate() {
        let target = (i as usize).min(sw);
        if target > at {
            if let Some(seg) = row.get(at * N..target * N) {
                for p in seg.chunks_exact(N) {
                    for (s, &b) in sum.iter_mut().zip(p) {
                        *s += u32::from(b);
                    }
                }
            }
            at = target;
        }
        let mut here = [0f32; N];
        for (h, &s) in here.iter_mut().zip(&sum) {
            *h = s as f32;
        }
        if f > 0.0
            && let Some(p) = row.get(at * N..at * N + N)
        {
            for (h, &b) in here.iter_mut().zip(p) {
                *h += f * f32::from(b);
            }
        }
        if o > 0
            && let Some(dst) = out.get_mut((o - 1) * N..o * N)
        {
            for ((d, n), pv) in dst.iter_mut().zip(here).zip(prev) {
                *d = (n - pv) * scale;
            }
        }
        prev = here;
    }
}

/// Average rows of `N` byte channels per pixel into the output pixels of `e` (`out` gets `ow * N` values,
/// 0 to 255): a running total along the row, read at the edges of the output pixels.
fn shrink_bytes<const N: usize>(row: &[u8], sw: usize, e: &Edges, out: &mut [f32]) {
    if sw <= EXACT_F32_ROW {
        return shrink_bytes_f32::<N>(row, sw, e, out);
    }
    let mut sum = [0u64; N];
    let mut pixels = row.chunks_exact(N).take(sw);
    // The pixels before the current one are in `sum`; `at` is the index of the current one.
    let mut at = 0usize;
    let mut current: Option<&[u8]> = pixels.next();
    let mut prev = [0f64; N];
    for (o, (&i, &f)) in e.index.iter().zip(&e.frac).enumerate() {
        let target = i.min(sw);
        while at < target {
            if let Some(p) = current {
                for (s, &b) in sum.iter_mut().zip(p) {
                    *s += u64::from(b);
                }
            }
            current = pixels.next();
            at += 1;
        }
        let mut here = [0f64; N];
        for (c, h) in here.iter_mut().enumerate() {
            *h = sum.get(c).copied().unwrap_or(0) as f64;
        }
        if f > 0.0
            && at == i
            && let Some(p) = current
        {
            for (h, &b) in here.iter_mut().zip(p) {
                *h += f * f64::from(b);
            }
        }
        if o > 0
            && let Some(dst) = out.get_mut((o - 1) * N..o * N)
        {
            for ((d, n), pv) in dst.iter_mut().zip(here).zip(prev) {
                *d = ((n - pv) * e.scale) as f32;
            }
        }
        prev = here;
    }
}

/// The same as [`shrink_bytes`] for any number of channels.
fn shrink_generic(row: &[u8], sw: usize, e: &Edges, n: usize, out: &mut [f32]) {
    for c in 0..n {
        let value = |i: usize| f64::from(row.get(i * n + c).copied().unwrap_or(0));
        // The running total at each edge, one channel at a time.
        let mut sum = 0f64;
        let mut at = 0usize;
        let mut prev = 0f64;
        for (o, edge) in e.index.iter().zip(&e.frac).enumerate() {
            let (&i, &f) = edge;
            while at < i.min(sw) {
                sum += value(at);
                at += 1;
            }
            let here = sum + if f > 0.0 { f * value(i) } else { 0.0 };
            if o > 0
                && let Some(d) = out.get_mut((o - 1) * n + c)
            {
                *d = ((here - prev) * e.scale) as f32;
            }
            prev = here;
        }
    }
}

/// The number of set bits before each byte of a packed row (and after the last), in `prefix`.
fn bit_prefix(row: &[u8], prefix: &mut Vec<u32>) {
    prefix.clear();
    let mut n = 0u32;
    prefix.push(0);
    for b in row {
        n += b.count_ones();
        prefix.push(n);
    }
}

/// The set bits before bit `p` of a packed row, given its [`bit_prefix`].
fn ones_before(row: &[u8], prefix: &[u32], p: usize) -> u32 {
    let (byte, bit) = (p / 8, p % 8);
    let whole = prefix.get(byte.min(prefix.len().saturating_sub(1))).copied().unwrap_or(0);
    if bit == 0 || byte >= row.len() {
        return whole;
    }
    let partial = row.get(byte).map_or(0, |b| (b >> (8 - bit)).count_ones());
    whole + partial
}

/// Average a row of one-bit samples into the output pixels of `e`: the share of the pixels that are set (0 to 1).
fn shrink_bits(row: &[u8], prefix: &mut Vec<u32>, e: &Edges, out: &mut [f32]) {
    bit_prefix(row, prefix);
    let scale = e.scale as f32;
    // (Counts of up to 2^24 pixels are exact in f32; a wider row is not meant to be a picture.)
    let integral = |edge: usize| -> f32 {
        let (i, f) = (e.index32.get(edge).copied().unwrap_or(0) as usize, e.frac32.get(edge).copied().unwrap_or(0.0));
        let own = if f > 0.0 && row.get(i / 8).is_some_and(|b| (b >> (7 - i % 8)) & 1 == 1) { f } else { 0.0 };
        ones_before(row, prefix, i) as f32 + own
    };
    let mut prev = integral(0);
    for (o, d) in out.iter_mut().enumerate() {
        let next = integral(o + 1);
        *d = (next - prev) * scale;
        prev = next;
    }
}

/// How the samples of an image turn into pixels.
enum Plan {
    /// A stencil mask: the colour is the fill colour, the alpha is the share of samples equal to `paint`.
    Stencil { paint: u8, rgb: [u8; 3] },
    /// One-bit samples, one component: the palette gives the colour of a 0 and of a 1.
    Bilevel { mix: Vec<[u8; 3]> },
    /// Grey, 8 bits, as it comes.
    Gray,
    /// RGB, 8 bits, as it comes.
    Rgb8,
    /// One component per pixel through a table (and perhaps a colour key).
    Table { table: Vec<[u8; 3]>, key: Option<Vec<(u16, u16)>> },
    /// RGB through a table for each component (and perhaps a colour key).
    Direct { luts: [Vec<u8>; 3], key: Option<Vec<(u16, u16)>> },
    /// Any other space: the raw samples are averaged, then decoded and converted.
    Comps { space: Arc<ColorSpace>, luts: Vec<Vec<f32>>, meter: Meter },
}

impl Plan {
    /// Channels averaged per pixel.
    fn channels(&self, ncomp: usize) -> usize {
        match self {
            Plan::Stencil { .. } | Plan::Bilevel { .. } | Plan::Gray => 1,
            Plan::Rgb8 => 3,
            Plan::Table { key, .. } | Plan::Direct { key, .. } => {
                if key.is_some() {
                    4
                } else {
                    3
                }
            }
            Plan::Comps { .. } => ncomp,
        }
    }
}

fn stencil_plan(decode: Option<&[f64]>, fill: [f32; 3]) -> Plan {
    let invert = decode.is_some_and(|d| d.first().copied().unwrap_or(0.0) > d.get(1).copied().unwrap_or(1.0));
    let rgb = [0, 1, 2].map(|i| (fill.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8);
    // With the default /Decode a 0 sample paints.
    Plan::Stencil { paint: u8::from(invert), rgb }
}

fn color_plan(s: &Samples, space: &Arc<ColorSpace>, decode: Option<&[f64]>, key: Option<Vec<(u16, u16)>>, meter: &Meter) -> Plan {
    let ncomp = s.ncomp;
    let maxv = if s.bpc >= 8 { 255.0 } else { f64::from((1u32 << s.bpc) - 1) };
    let levels = if s.bpc >= 8 { 256usize } else { 1usize << s.bpc };
    let decode: Vec<f64> = match decode {
        Some(d) if d.len() == ncomp * 2 => d.to_vec(),
        _ => space.default_decode(s.bpc),
    };
    // Per component: raw sample to decoded value.
    let luts: Vec<Vec<f32>> = (0..ncomp)
        .map(|c| {
            let lo = decode.get(2 * c).copied().unwrap_or(0.0);
            let hi = decode.get(2 * c + 1).copied().unwrap_or(1.0);
            (0..levels).map(|v| (lo + v as f64 * (hi - lo) / maxv) as f32).collect()
        })
        .collect();
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    // The samples are the values: nothing to look up.
    let plain = key.is_none() && s.bpc >= 8 && decode.iter().enumerate().all(|(i, &d)| d == (i % 2) as f64);
    if plain && ncomp == 1 && matches!(**space, ColorSpace::Gray) {
        return Plan::Gray;
    }
    if plain && ncomp == 3 && matches!(**space, ColorSpace::Rgb) {
        return Plan::Rgb8;
    }
    if ncomp == 1 {
        let table: Vec<[u8; 3]> = luts
            .first()
            .map(|l| l.iter().map(|&v| space.to_rgb(&[f64::from(v)], meter).map(to8)).collect())
            .unwrap_or_default();
        if s.bpc == 1 && key.is_none() {
            let (zero, one) = (table.first().copied().unwrap_or([0; 3]), table.get(1).copied().unwrap_or([255; 3]));
            // The colour for each of 256 shares of set bits.
            let mix = (0..256)
                .map(|i| {
                    let p = i as f32 / 255.0;
                    let at = |lo: u8, hi: u8| (f32::from(lo) + (f32::from(hi) - f32::from(lo)) * p + 0.5) as u8;
                    [at(zero[0], one[0]), at(zero[1], one[1]), at(zero[2], one[2])]
                })
                .collect();
            return Plan::Bilevel { mix };
        }
        return Plan::Table { table, key };
    }
    if matches!(**space, ColorSpace::Rgb) && ncomp == 3 {
        let one = |c: usize| -> Vec<u8> { luts.get(c).map(|l| l.iter().map(|&v| to8(v)).collect()).unwrap_or_default() };
        return Plan::Direct { luts: [one(0), one(1), one(2)], key };
    }
    Plan::Comps { space: space.clone(), luts, meter: meter.clone() }
}

/// Turn a row of raw sample values into the channels the plan averages.
fn channels_of(plan: &Plan, key_test: &dyn Fn(&[u8]) -> bool, raw: &[u8], ncomp: usize, out: &mut Vec<u8>) {
    out.clear();
    match plan {
        Plan::Table { table, key } => {
            for &v in raw {
                let [r, g, b] = table.get(usize::from(v)).copied().unwrap_or([0; 3]);
                if key.is_some() && key_test(std::slice::from_ref(&v)) {
                    out.extend_from_slice(&[0, 0, 0, 0]);
                } else if key.is_some() {
                    out.extend_from_slice(&[r, g, b, 255]);
                } else {
                    out.extend_from_slice(&[r, g, b]);
                }
            }
        }
        Plan::Direct { luts: [tr, tg, tb], key } => {
            for px in raw.chunks_exact(3) {
                if let [r, g, b] = px {
                    let (r, g, b) = (
                        tr.get(usize::from(*r)).copied().unwrap_or(0),
                        tg.get(usize::from(*g)).copied().unwrap_or(0),
                        tb.get(usize::from(*b)).copied().unwrap_or(0),
                    );
                    if key.is_some() && key_test(px) {
                        out.extend_from_slice(&[0, 0, 0, 0]);
                    } else if key.is_some() {
                        out.extend_from_slice(&[r, g, b, 255]);
                    } else {
                        out.extend_from_slice(&[r, g, b]);
                    }
                }
            }
        }
        _ => {
            let _ = ncomp;
            out.extend_from_slice(raw);
        }
    }
}

/// Does the plan average the raw samples as they are?
fn is_raw(plan: &Plan) -> bool {
    matches!(plan, Plan::Comps { .. } | Plan::Gray | Plan::Rgb8)
}

/// The picture's pixels shrunk to `ow` by `oh` (at most the size it has), as straight RGBA.
fn resample(s: &Samples, plan: &Plan, key: Option<&[(u16, u16)]>, ow: usize, oh: usize) -> Vec<u8> {
    let mut out = vec![0u8; ow * oh * 4];
    // Not shrunk at all (a picture shown at its own size, or decoded down to it), 8-bit grey or RGB as it comes: the
    // samples are the pixels, no averaging.
    if (ow, oh) == (s.w, s.h) && s.bpc == 8 && key.is_none() {
        let ncomp = match plan {
            Plan::Gray if s.ncomp == 1 => 1,
            Plan::Rgb8 if s.ncomp == 3 => 3,
            _ => 0,
        };
        if ncomp != 0 {
            for (y, dst) in out.chunks_exact_mut(ow * 4).enumerate() {
                let Some(row) = s.data.get(y * s.stride..y * s.stride + ow * ncomp) else { continue };
                if ncomp == 1 {
                    for (px, &g) in dst.chunks_exact_mut(4).zip(row) {
                        px.copy_from_slice(&[g, g, g, 255]);
                    }
                } else {
                    for (px, rgb) in dst.chunks_exact_mut(4).zip(row.chunks_exact(3)) {
                        if let [r, g, b] = rgb {
                            px.copy_from_slice(&[*r, *g, *b, 255]);
                        }
                    }
                }
            }
            return out;
        }
    }
    // A big picture is done in bands of output rows, a few at a time (each band reads the source rows it needs).
    let threads = if s.w * s.h >= 2_000_000 && oh >= 64 { std::thread::available_parallelism().map_or(1, |n| n.get()).min(4) } else { 1 };
    if threads <= 1 {
        resample_band(s, plan, key, (ow, oh), (0, oh), &mut out);
        return out;
    }
    let band = oh.div_ceil(threads);
    std::thread::scope(|scope| {
        for (i, chunk) in out.chunks_mut(band * ow * 4).enumerate() {
            let rows = (i * band, ((i + 1) * band).min(oh));
            scope.spawn(move || resample_band(s, plan, key, (ow, oh), rows, chunk));
        }
    });
    out
}

/// The output rows `rows.0..rows.1` of [`resample`], into `out` (which starts at the first of them).
fn resample_band(s: &Samples, plan: &Plan, key: Option<&[(u16, u16)]>, (ow, oh): (usize, usize), rows_wanted: (usize, usize), out: &mut [u8]) {
    let (sw, sh) = (s.w, s.h);
    let (o_lo, o_hi) = rows_wanted;
    let nch = plan.channels(s.ncomp);
    let col_edges = edges(sw, ow);
    let key_test = |px: &[u8]| -> bool {
        key.is_some_and(|ranges| {
            px.iter().zip(ranges).all(|(&v, &(lo, hi))| {
                // The key is in raw sample values; 16-bit samples were cut to the high byte.
                let v = u16::from(v);
                let (lo, hi) = if s.bpc == 16 { (lo >> 8, hi >> 8) } else { (lo, hi) };
                v >= lo && v <= hi
            })
        })
    };
    // Two output rows are being added up at a time: the one the source row is in, and the next.
    let mut rows = [vec![0f32; ow * nch], vec![0f32; ow * nch]];
    let mut hrow = vec![0f32; ow * nch];
    let mut buf = Vec::new();
    let mut chans = Vec::new();
    let mut prefix = Vec::new();
    let mut tints = TintCache::default();
    let mut oy = o_lo;
    // The source rows this band touches: source row y is [y*oh, (y+1)*oh), output row o is [o*sh, (o+1)*sh).
    for y in (o_lo * sh / oh)..((o_hi * sh).div_ceil(oh)).min(sh) {
        // This source row, shrunk horizontally.
        match plan {
            Plan::Stencil { .. } | Plan::Bilevel { .. } => {
                let row = s.data.get(y * s.stride..(y + 1) * s.stride).unwrap_or(&[]);
                shrink_bits(row, &mut prefix, &col_edges, &mut hrow);
            }
            _ => {
                let raw = unpack_row(s, y, &mut buf);
                let row: &[u8] = if is_raw(plan) {
                    raw
                } else {
                    channels_of(plan, &key_test, raw, s.ncomp, &mut chans);
                    &chans
                };
                if ow == sw {
                    for (d, &v) in hrow.iter_mut().zip(row) {
                        *d = f32::from(v);
                    }
                } else {
                    match nch {
                        1 => shrink_bytes::<1>(row, sw, &col_edges, &mut hrow),
                        2 => shrink_bytes::<2>(row, sw, &col_edges, &mut hrow),
                        3 => shrink_bytes::<3>(row, sw, &col_edges, &mut hrow),
                        4 => shrink_bytes::<4>(row, sw, &col_edges, &mut hrow),
                        n => shrink_generic(row, sw, &col_edges, n, &mut hrow),
                    }
                }
            }
        }
        // Add it to the output rows it covers.
        let (start, end) = (y * oh, (y + 1) * oh);
        let mut o = start / sh;
        while o * sh < end && o < oh {
            let weight = (end.min((o + 1) * sh) - start.max(o * sh)) as f32 / sh as f32;
            // o is oy or oy + 1 (or the row above the band: not ours).
            if let Some(acc) = o.checked_sub(oy).and_then(|d| rows.get_mut(d)) {
                for (a, &h) in acc.iter_mut().zip(&hrow) {
                    *a += h * weight;
                }
            }
            o += 1;
        }
        // The output row `oy` is done when the source rows end at its end.
        if end >= (oy + 1) * sh || y + 1 == sh {
            if let (Some(dst), Some(done)) = (out.get_mut((oy - o_lo) * ow * 4..(oy - o_lo + 1) * ow * 4), rows.first()) {
                finish_row(plan, done, ow, nch, dst, &mut tints);
            }
            let [a, b] = &mut rows;
            std::mem::swap(a, b);
            b.fill(0.0);
            oy += 1;
            if oy >= o_hi {
                break;
            }
        }
    }
}

/// One finished output row of averaged channels as straight RGBA.
fn finish_row(plan: &Plan, acc: &[f32], ow: usize, nch: usize, dst: &mut [u8], tints: &mut TintCache) {
    let to8 = |v: f32| (v + 0.5) as u8;
    match plan {
        Plan::Gray => {
            for (px, &v) in dst.chunks_exact_mut(4).zip(acc).take(ow) {
                let g = to8(v);
                px.copy_from_slice(&[g, g, g, 255]);
            }
            return;
        }
        Plan::Rgb8 => {
            for (px, a) in dst.chunks_exact_mut(4).zip(acc.chunks_exact(3)).take(ow) {
                if let [r, g, b] = a {
                    px.copy_from_slice(&[to8(*r), to8(*g), to8(*b), 255]);
                }
            }
            return;
        }
        Plan::Bilevel { mix } => {
            for (px, &v) in dst.chunks_exact_mut(4).zip(acc).take(ow) {
                let [r, g, b] = mix.get((v.clamp(0.0, 1.0) * 255.0 + 0.5) as usize).copied().unwrap_or([0; 3]);
                px.copy_from_slice(&[r, g, b, 255]);
            }
            return;
        }
        Plan::Stencil { paint, rgb } => {
            for (px, &v) in dst.chunks_exact_mut(4).zip(acc).take(ow) {
                let share = if *paint == 1 { v } else { 1.0 - v };
                px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], to8(share.clamp(0.0, 1.0) * 255.0)]);
            }
            return;
        }
        _ => {}
    }
    for (x, px) in dst.chunks_exact_mut(4).enumerate().take(ow) {
        let a = acc.get(x * nch..x * nch + nch).unwrap_or(&[]);
        let v = |i: usize| a.get(i).copied().unwrap_or(0.0);
        match plan {
            Plan::Stencil { .. } | Plan::Bilevel { .. } | Plan::Gray | Plan::Rgb8 => {}
            Plan::Table { key: None, .. } | Plan::Direct { key: None, .. } => {
                px.copy_from_slice(&[to8(v(0)), to8(v(1)), to8(v(2)), 255]);
            }
            Plan::Table { .. } | Plan::Direct { .. } => {
                // Premultiplied by the colour key's alpha: back to straight colour.
                let alpha = v(3).clamp(0.0, 255.0);
                if alpha > 0.0 {
                    px.copy_from_slice(&[to8(v(0) * 255.0 / alpha), to8(v(1) * 255.0 / alpha), to8(v(2) * 255.0 / alpha), to8(alpha)]);
                } else {
                    px.copy_from_slice(&[0, 0, 0, 0]);
                }
            }
            Plan::Comps { space, luts, meter } => {
                let [r, g, b] = comps_pixel(space, luts, meter, a, tints);
                px.copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
}

/// Colours already worked out for the pixels of a spot-colour image (DeviceN and the like, whose tint
/// function is far slower than a table), by the whole sample values they came from.
#[derive(Default)]
struct TintCache(HashMap<u64, [u8; 3]>);

/// Entries a [`TintCache`] holds before it starts over.
const TINT_CACHE_ENTRIES: usize = 1 << 17;

/// The colour (0 to 255 each) of a pixel whose averaged raw samples are `a`, in a space that is not a device space.
fn comps_pixel(space: &ColorSpace, luts: &[Vec<f32>], meter: &Meter, a: &[f32], tints: &mut TintCache) -> [u8; 3] {
    let to8 = |v: f32| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    let raw = |c: usize| a.get(c).copied().unwrap_or(0.0);
    // A tint function costs hundreds of steps where a table lookup costs one: remember what it gave for
    // each combination of whole sample values (at most 8 components fit the key), and use those values.
    if matches!(space, ColorSpace::Tint { .. }) && luts.len() <= 8 {
        let whole: Vec<usize> = luts.iter().enumerate().map(|(c, lut)| ((raw(c) + 0.5).max(0.0) as usize).min(lut.len().saturating_sub(1))).collect();
        let key = whole.iter().fold(0u64, |k, &i| (k << 8) | (i as u64 & 0xFF));
        if let Some(rgb) = tints.0.get(&key) {
            return *rgb;
        }
        let comps: Vec<f64> = luts.iter().zip(&whole).map(|(lut, &i)| f64::from(lut.get(i).copied().unwrap_or(0.0))).collect();
        let rgb = space.to_rgb(&comps, meter).map(to8);
        if tints.0.len() >= TINT_CACHE_ENTRIES {
            tints.0.clear();
        }
        tints.0.insert(key, rgb);
        return rgb;
    }
    let comps: Vec<f64> = luts
        .iter()
        .enumerate()
        .map(|(c, lut)| {
            // The decoded value of an averaged raw sample: the table is linear, so interpolate.
            let raw = raw(c).clamp(0.0, (lut.len().saturating_sub(1)) as f32);
            let i = raw.floor() as usize;
            let f = raw - i as f32;
            let lo = lut.get(i).copied().unwrap_or(0.0);
            let hi = lut.get(i + 1).copied().unwrap_or(lo);
            f64::from(lo + (hi - lo) * f)
        })
        .collect();
    space.to_rgb(&comps, meter).map(to8)
}

/// An explicit mask or a soft mask as alpha for `ow` by `oh` pixels (`None`: the mask is not usable).
fn mask_alpha(env: &Env<'_>, mask: &Stream, stencil: bool, ow: usize, oh: usize, warnings: &mut Warnings) -> Result<Option<Vec<u8>>> {
    let doc = env.doc;
    let mw = int(env, &mask.dict, "Width").unwrap_or(0);
    let mh = int(env, &mask.dict, "Height").unwrap_or(0);
    if mw <= 0 || mh <= 0 || (mw as u64) * (mh as u64) > MAX_IMAGE_PIXELS {
        return Ok(None);
    }
    env.charge_pixels((mw as u64) * (mh as u64))?;
    let inner = Env { doc, lookup: env.lookup, fill: [0.0; 3], target: (ow, oh), meter: env.meter, pixels_left: env.pixels_left, work: env.work, globals: env.globals };
    let stencil = stencil || matches!(get(&mask.dict, env, "ImageMask").as_deref(), Some(Object::Bool(true)));
    let bpc = if stencil { 1 } else { int(env, &mask.dict, "BitsPerComponent").unwrap_or(8) };
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return Ok(None);
    }
    let Some(samples) = read_samples(&inner, mask, mw as usize, mh as usize, bpc as u32, (Some(1), false), warnings)? else { return Ok(None) };
    let decode = get(&mask.dict, env, "Decode").and_then(|o| number_list(env, &o));
    // Shrunk to our size (or left as it is when smaller), then each pixel of ours takes the mask pixel at its middle.
    let (mut rw, mut rh) = (samples.w.min(ow), samples.h.min(oh));
    if rw * rh > MAX_MASK_PIXELS {
        let f = (MAX_MASK_PIXELS as f64 / (rw * rh) as f64).sqrt();
        rw = ((rw as f64 * f) as usize).max(1);
        rh = ((rh as f64 * f) as usize).max(1);
    }
    let small = if stencil {
        // In an explicit mask a 0 sample paints; the stencil plan gives alpha 255 where it does.
        resample(&samples, &stencil_plan(decode.as_deref(), [1.0; 3]), None, rw, rh)
    } else {
        let plan = color_plan(&samples, &Arc::new(ColorSpace::Gray), decode.as_deref(), None, env.meter);
        // A grey level is the alpha: the red channel of the grey picture.
        let mut px = resample(&samples, &plan, None, rw, rh);
        for p in px.chunks_exact_mut(4) {
            if let [r, _, _, a] = p {
                *a = *r;
            }
        }
        px
    };
    // The samples and the four-byte pixels are not needed any more: keep the alpha alone.
    drop(samples);
    let plane: Vec<u8> = small.chunks_exact(4).map(|p| p.get(3).copied().unwrap_or(255)).collect();
    drop(small);
    if (rw, rh) == (ow, oh) {
        return Ok(Some(plane));
    }
    let mut alpha = vec![255u8; ow * oh];
    for (oy, row) in alpha.chunks_exact_mut(ow).enumerate() {
        let sy = ((oy * 2 + 1) * rh / (oh * 2)).min(rh.saturating_sub(1));
        for (ox, a) in row.iter_mut().enumerate() {
            let sx = ((ox * 2 + 1) * rw / (ow * 2)).min(rw.saturating_sub(1));
            *a = plane.get(sy * rw + sx).copied().unwrap_or(255);
        }
    }
    Ok(Some(alpha))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize) -> Samples {
        let mut x = 12345u32;
        let data = (0..w * h)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect();
        Samples { w, h, bpc: 8, ncomp: 1, stride: w, data, alpha: None }
    }

    #[test]
    fn bands_give_the_same_pixels_as_one_pass() {
        // Big enough to be cut into bands; sizes that do not divide each other.
        let s = gray(2101, 1051);
        let plan = color_plan(&s, &Arc::new(ColorSpace::Gray), None, None, &Meter::new(u64::MAX));
        let (ow, oh) = (333, 151);
        let whole = resample(&s, &plan, None, ow, oh);
        let mut one = vec![0u8; ow * oh * 4];
        resample_band(&s, &plan, None, (ow, oh), (0, oh), &mut one);
        assert!(whole == one);
        // And the average is the average: the mean of the picture is kept.
        let mean_in = s.data.iter().map(|&v| f64::from(v)).sum::<f64>() / s.data.len() as f64;
        let mean_out = whole.chunks_exact(4).map(|p| f64::from(p[0])).sum::<f64>() / (ow * oh) as f64;
        assert!((mean_in - mean_out).abs() < 1.0, "{mean_in} {mean_out}");
    }

    #[test]
    fn a_jpeg_is_charged_for_the_pixels_it_has_not_the_ones_it_says() {
        // The dictionary says 1 by 1; the file is 64 by 64.
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).expect("fixture");
        let doc = Document::from_bytes(crate::testutil::sample_pdf()).expect("opens");
        let lookup = |_: &[u8]| None;
        let meter = Meter::new(1000);
        let left = Cell::new(100);
        let work = Work::new();
        let globals = jbig2::GlobalsCache::default();
        let env = Env { doc: &doc, lookup: &lookup, fill: [0.0; 3], target: (8, 8), meter: &meter, pixels_left: &left, work: &work, globals: &globals };
        let mut dict = Dict::new();
        dict.set("Width", Object::Integer(1));
        dict.set("Height", Object::Integer(1));
        dict.set("BitsPerComponent", Object::Integer(8));
        dict.set("ColorSpace", Object::from("DeviceRGB"));
        dict.set("Filter", Object::from("DCTDecode"));
        let stream = Stream { dict, data: jpeg };
        let mut warnings = Warnings::default();
        assert!(matches!(load(&env, &stream, &mut warnings), Err(Error::Limit(_))));
        // With the pixels it has, it loads, and they are spent.
        left.set(5000);
        assert!(matches!(load(&env, &stream, &mut warnings), Ok(Loaded::Image { .. })));
        assert_eq!(left.get(), 5000 - 64 * 64);
    }

    /// An image stream of a JPEG 2000 fixture with the given extra dictionary entries, loaded for a `target` size.
    fn load_jpx(name: &str, extra: &[(&str, Object)], target: (usize, usize), work: &Work) -> (Result<Loaded>, Warnings) {
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/jpx_fixtures/").to_string() + name).expect("fixture");
        let doc = Document::from_bytes(crate::testutil::sample_pdf()).expect("opens");
        let lookup = |_: &[u8]| None;
        let meter = Meter::new(1000);
        let left = Cell::new(1 << 20);
        let globals = jbig2::GlobalsCache::default();
        let env = Env { doc: &doc, lookup: &lookup, fill: [0.0; 3], target, meter: &meter, pixels_left: &left, work, globals: &globals };
        let mut dict = Dict::new();
        dict.set("Width", Object::Integer(61));
        dict.set("Height", Object::Integer(47));
        dict.set("Filter", Object::from("JPXDecode"));
        for (k, v) in extra {
            dict.set(*k, v.clone());
        }
        let mut warnings = Warnings::default();
        let r = load(&env, &Stream { dict, data }, &mut warnings);
        (r, warnings)
    }

    fn raw_fixture(name: &str) -> Vec<u8> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/jpx_fixtures/").to_string() + name).expect("fixture")
    }

    #[test]
    fn a_jpeg_2000_image_is_drawn_at_the_size_the_page_shows() {
        let work = Work::new();
        let raw = raw_fixture("rgb_lossless.raw");
        // Full size: the picture as it is.
        let (r, w) = load_jpx("rgb_lossless.jp2", &[], (61, 47), &work);
        let Ok(Loaded::Image { pixmap, opaque }) = r else { panic!("not an image") };
        assert!(opaque && w.list.is_empty());
        assert_eq!((pixmap.width(), pixmap.height()), (61, 47));
        for (px, rgb) in pixmap.data().chunks_exact(4).zip(raw.chunks_exact(3)) {
            assert_eq!(&px[..3], rgb);
        }
        // A fifth of the size: decoded at a quarter (two levels fewer) and shrunk to fit; far less work.
        let small = Work::new();
        let (r, _) = load_jpx("rgb_lossless.jp2", &[], (13, 10), &small);
        let Ok(Loaded::Image { pixmap, .. }) = r else { panic!("not an image") };
        assert_eq!((pixmap.width(), pixmap.height()), (13, 10));
        assert!(small.used() * 2.0 < work.used(), "{} {}", small.used(), work.used());
    }

    #[test]
    fn a_jpeg_2000_image_takes_its_colour_space_from_the_dictionary_first() {
        let work = Work::new();
        let raw = raw_fixture("rgb_lossless.raw");
        // /DeviceGray over three channels: the first one is the grey level.
        let (r, _) = load_jpx("rgb_lossless.jp2", &[("ColorSpace", Object::from("DeviceGray"))], (61, 47), &work);
        let Ok(Loaded::Image { pixmap, .. }) = r else { panic!("not an image") };
        for (px, rgb) in pixmap.data().chunks_exact(4).zip(raw.chunks_exact(3)) {
            assert_eq!(&px[..3], &[rgb[0]; 3]);
        }
        // The dictionary's bits per component are of no account (7.4.9).
        let (r, _) = load_jpx("rgb_lossless.jp2", &[("BitsPerComponent", Object::Integer(12))], (61, 47), &work);
        assert!(matches!(r, Ok(Loaded::Image { .. })));
    }

    #[test]
    fn decode_is_ignored_for_a_jpeg_2000_image() {
        // 7.4.9: /Decode is ignored, whatever it says.
        let work = Work::new();
        let raw = raw_fixture("rgb_lossless.raw");
        let inverted = Object::Array([1, 0, 1, 0, 1, 0].map(Object::Integer).to_vec());
        let (r, _) = load_jpx("rgb_lossless.jp2", &[("Decode", inverted)], (61, 47), &work);
        let Ok(Loaded::Image { pixmap, .. }) = r else { panic!("not an image") };
        for (px, rgb) in pixmap.data().chunks_exact(4).zip(raw.chunks_exact(3)) {
            assert_eq!(&px[..3], rgb);
        }
    }

    #[test]
    fn a_jpeg_2000_soft_mask_is_decoded_as_big_as_the_picture_it_masks() {
        // A grey picture of 61 by 47 with a JPEG 2000 grey soft mask of the same size: the mask must come out in full,
        // not at the smallest size its levels allow.
        let work = Work::new();
        let mask = raw_fixture("gray_lossless.raw");
        let mut mdict = Dict::new();
        mdict.set("Width", Object::Integer(61));
        mdict.set("Height", Object::Integer(47));
        mdict.set("ColorSpace", Object::from("DeviceGray"));
        mdict.set("Filter", Object::from("JPXDecode"));
        let jp2 = raw_fixture("gray_lossless.jp2");
        let (r, _) = load_jpx("rgb_lossless.jp2", &[("SMask", Object::Stream(Stream { dict: mdict, data: jp2 })), ("ColorSpace", Object::from("DeviceGray"))], (61, 47), &work);
        let Ok(Loaded::Image { pixmap, opaque }) = r else { panic!("not an image") };
        assert!(!opaque);
        for (px, &a) in pixmap.data().chunks_exact(4).zip(&mask) {
            assert_eq!(px[3], a);
        }
    }

    #[test]
    fn the_opacity_channel_of_a_jpeg_2000_image_is_used_when_the_dictionary_asks() {
        let work = Work::new();
        let raw = raw_fixture("rgba_lossless.raw");
        let (r, _) = load_jpx("rgba_lossless.jp2", &[("SMaskInData", Object::Integer(1))], (61, 47), &work);
        let Ok(Loaded::Image { pixmap, opaque }) = r else { panic!("not an image") };
        assert!(!opaque);
        for (px, rgba) in pixmap.data().chunks_exact(4).zip(raw.chunks_exact(4)) {
            let a = u32::from(rgba[3]);
            assert_eq!(u32::from(px[3]), a);
            // The pixmap holds the colour multiplied by the alpha.
            assert_eq!(u32::from(px[0]), (u32::from(rgba[0]) * a + 127) / 255);
        }
        // Without /SMaskInData the channel is left alone.
        let (r, _) = load_jpx("rgba_lossless.jp2", &[], (61, 47), &work);
        let Ok(Loaded::Image { opaque, .. }) = r else { panic!("not an image") };
        assert!(opaque);
    }

    #[test]
    fn a_jpeg_2000_image_that_cannot_be_decoded_is_a_grey_block_with_a_warning() {
        let doc = Document::from_bytes(crate::testutil::sample_pdf()).expect("opens");
        let lookup = |_: &[u8]| None;
        let meter = Meter::new(1000);
        let left = Cell::new(1 << 20);
        let work = Work::new();
        let globals = jbig2::GlobalsCache::default();
        let env = Env { doc: &doc, lookup: &lookup, fill: [0.0; 3], target: (8, 8), meter: &meter, pixels_left: &left, work: &work, globals: &globals };
        let mut dict = Dict::new();
        dict.set("Width", Object::Integer(8));
        dict.set("Height", Object::Integer(8));
        dict.set("Filter", Object::from("JPXDecode"));
        let mut warnings = Warnings::default();
        let r = load(&env, &Stream { dict, data: vec![0xFF, 0x4F, 0xFF, 0x51, 0, 3, 0] }, &mut warnings);
        assert!(matches!(r, Ok(Loaded::Placeholder)));
        assert!(!warnings.list.is_empty());
        // And with no work left the page is told so.
        let spent = Work::with_allowance(10.0);
        let (r, _) = load_jpx("rgb_lossless.jp2", &[], (61, 47), &spent);
        assert!(matches!(r, Err(Error::Limit(_))));
    }

    #[test]
    fn one_bit_rows_average_like_bytes() {
        // A 1-bit picture and the same picture as grey bytes shrink to the same thing (to a level).
        let (w, h) = (97usize, 61usize);
        let stride = w.div_ceil(8);
        let mut bits = vec![0u8; stride * h];
        let mut bytes = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let set = (x * 7 + y * 3) % 5 < 2;
                if set {
                    bits[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
                bytes.push(if set { 255 } else { 0 });
            }
        }
        let b = Samples { w, h, bpc: 1, ncomp: 1, stride, data: bits, alpha: None };
        let g = Samples { w, h, bpc: 8, ncomp: 1, stride: w, data: bytes, alpha: None };
        let space = Arc::new(ColorSpace::Gray);
        let (ob, og) = (resample(&b, &color_plan(&b, &space, None, None, &Meter::new(u64::MAX)), None, 40, 25), resample(&g, &color_plan(&g, &space, None, None, &Meter::new(u64::MAX)), None, 40, 25));
        for (p, q) in ob.chunks_exact(4).zip(og.chunks_exact(4)) {
            assert!(p[0].abs_diff(q[0]) <= 1, "{} {}", p[0], q[0]);
        }
    }
}
