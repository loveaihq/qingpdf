//! Pages as pictures, and the pictures that pages show as files of their own (2026-10-21).
//!
//! A page is drawn as the reader shows it (the annotations and form fields too, nothing recoloured) a band of rows at a time and each band is
//! written as soon as it is drawn, into a PNG ([`PageWriter`]) or a JPEG ([`Banded`]) that goes straight to the file, so the memory a page takes
//! is that of one band whatever the resolution. The picture is the one `qingpdf render` makes: the same pieces of the same drawing.
//!
//! The pictures of a page are the image XObjects it shows (the ones [`TextExtractor::page_layout`] finds, forms included; inline images have no
//! object and are left out). A picture that is one JPEG or one JPEG 2000 stream is saved as it is, byte for byte; any other is decoded and saved
//! as a PNG, with its soft mask as the alpha channel. The file names are made here too.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::idmap::IdSet;
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::render::Renderer;
use crate::render::export::{PageWriter, decode_image, png};
use crate::render::work::Work;
use crate::squeeze::jpeg::{self, Banded};
use crate::text::TextExtractor;

/// The kinds of picture a page is saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    /// With this quality, 1 to 100.
    Jpeg(u8),
}

impl ImageFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg(_) => "jpg",
        }
    }
}

/// The resolutions the reader offers, in dots to the inch (any from 1 to 2400 can be asked for on the command line).
pub const DPI_CHOICES: [u32; 4] = [72, 150, 300, 600];
/// The quality of a JPEG when nothing says otherwise.
pub const DEFAULT_QUALITY: u8 = 90;
/// About how many pixels of a page are drawn and held at a time: the least a piece of the reader's plan is (a band is 4 MB of canvas, whatever the page
/// and the resolution), so the pages written by the reader and by `qingpdf render` are cut into the same bands and come out as the same bytes. (Cutting
/// into more bands costs a pass through the page's content for each: about 15 per cent more time than 4 million pixels did.)
pub const BAND_PIXELS: u64 = 1 << 20;
/// The most pixels one page may be written with (an A2 sheet at 600 dots to the inch is 139 million).
pub const MAX_EXPORT_PIXELS: u64 = 250_000_000;
/// The most pictures that one run saves (the same picture on many pages is one).
pub const MAX_PICTURES: usize = 20_000;
/// The most bytes of pictures one run saves.
pub const MAX_PICTURE_BYTES: u64 = 4 << 30;
/// The most pixels of a picture the reader decodes to save it as a PNG (a bit more than an A4 page scanned at 600 dots to the inch): the picture takes
/// about ten bytes a pixel while it is decoded and written, and an allocation that fails ends the program, with the edits not saved. A picture that
/// is saved as it is (a JPEG or JPEG 2000 stream) takes no memory to speak of and has no such limit. The command line has the decoder's own limit.
pub const READER_PICTURE_PIXELS: u64 = 40_000_000;
/// How many files of one name `write_new_file` tries (`name`, `name-2`, ... `name-1000`) before it gives up.
const MAX_CLASH: usize = 1_000;

/// The name a file of pictures starts with: the name of the PDF without `.pdf`, with the characters Windows does not allow in a name (and the control
/// characters) made `_`, at most 120 characters, never empty.
#[inline(never)]
pub fn file_stem(file_name: &str) -> String {
    let base = file_name.rsplit(['\\', '/']).next().unwrap_or("");
    let stem = match base.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("pdf") && !stem.is_empty() => stem,
        _ => base,
    };
    let clean: String = stem.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).take(120).collect();
    let clean = clean.trim().trim_end_matches('.').trim_end().to_string();
    if clean.is_empty() { "page".to_string() } else { clean }
}

/// The name of the file page `page` (from 0) is saved as: `report-3.png`.
pub fn page_file_name(stem: &str, page: usize, format: ImageFormat) -> String {
    format!("{stem}-{}.{}", page + 1, format.ext())
}

/// The name, without the extension, of the `n`-th picture (from 1) saved from page `page` (from 0): `report-p3-2`.
pub fn picture_file_stem(stem: &str, page: usize, n: usize) -> String {
    format!("{stem}-p{}-{n}", page + 1)
}

/// The name of the `n`-th picture (from 1) saved from page `page` (from 0): `report-p3-2.jpg`.
pub fn picture_file_name(stem: &str, page: usize, n: usize, ext: &str) -> String {
    format!("{}.{ext}", picture_file_stem(stem, page, n))
}

/// Write the file `path` all at once: into a file of its own in the same folder, which then takes the place of `path`. `make` gets the file and
/// gives it back when it has written all. Nothing is left half written.
#[inline(never)]
pub fn write_file(path: &Path, make: impl FnOnce(BufWriter<File>) -> Result<BufWriter<File>>) -> Result<()> {
    let name = path.file_name().ok_or_else(|| Error::Invalid("the file has no name".to_string()))?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".qingpdf-{}.tmp", std::process::id()));
    let temp = dir.join(temp_name);
    let done = (|| -> Result<()> {
        let file = BufWriter::with_capacity(1 << 16, File::create(&temp)?);
        let mut file = make(file)?;
        file.flush()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    done
}

/// Save `data` as a file of its own in `dir` that nothing was at: `<stem>.<ext>`, or when a file of that name is there (even one that is not a file, or a
/// link that leads nowhere), `<stem>-2.<ext>`, `<stem>-3.<ext>` and so on. The file is made with `create_new`, so one that appears meanwhile is not written over
/// either; a failure part-way takes the file away again. The path written is given back.
#[inline(never)]
pub fn write_new_file(dir: &Path, stem: &str, ext: &str, data: &[u8]) -> Result<PathBuf> {
    for n in 1..=MAX_CLASH {
        let path = if n == 1 { dir.join(format!("{stem}.{ext}")) } else { dir.join(format!("{stem}-{n}.{ext}")) };
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            // (Windows says "access denied", not "exists", where the name is a folder's.)
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists || std::fs::symlink_metadata(&path).is_ok() => continue,
            Err(e) => return Err(e.into()),
        };
        let written = file.write_all(data).and_then(|()| file.flush());
        drop(file);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&path);
            return Err(e.into());
        }
        return Ok(path);
    }
    Err(Error::Invalid(format!("there are already {MAX_CLASH} files named like {stem}.{ext} in the folder")))
}

/// What came of writing a page: its size in pixels, and the characters drawn as boxes (and why, as `Bitmap::boxed_causes` says; counted once for each
/// band the character is in).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub width: u32,
    pub height: u32,
    pub boxed: usize,
    pub causes: [usize; 3],
}

enum Sink<W: Write> {
    Png(PageWriter<W>),
    Jpeg(Box<Banded<W>>),
}

/// Draw `page` at `dpi` and write it to `sink` as `format` says, `band_pixels` pixels (about) at a time; the sink is given back, with the size of the
/// picture ([`Written`]). The page is drawn the way `qingpdf render` draws it (with the annotations): one that goes wrong part-way, or is stopped, is an error.
#[inline(never)]
pub fn write_page<W: Write>(renderer: &mut Renderer<'_>, page: &Page, dpi: f64, format: ImageFormat, band_pixels: u64, sink: W) -> Result<(W, Written)> {
    let (w, h) = Renderer::page_pixels(page, dpi, 0);
    if w.saturating_mul(h) > MAX_EXPORT_PIXELS {
        return Err(Error::Limit(format!("the page would be {w} by {h} pixels at {dpi} dpi; the most is {MAX_EXPORT_PIXELS} pixels (use a lower --dpi)")));
    }
    let (width, height) = (u32::try_from(w).unwrap_or(u32::MAX), u32::try_from(h).unwrap_or(u32::MAX));
    let unit = if matches!(format, ImageFormat::Jpeg(_)) { 16 } else { 1 };
    let rows_per = (band_pixels / w.max(1)).max(1) / unit * unit;
    let rows_per = rows_per.max(unit);
    let mut out = match format {
        ImageFormat::Png => Sink::Png(PageWriter::new(sink, width, height)?),
        ImageFormat::Jpeg(q) => Sink::Jpeg(Box::new(Banded::new(sink, (w as usize, h as usize), 3, q)?)),
    };
    let mut written = Written { width, height, ..Written::default() };
    let mut y = 0u64;
    while y < h {
        let rows = rows_per.min(h - y);
        let drawn = renderer.render_partial(page, dpi, 0, Some([0, y as u32, width, rows as u32]))?;
        if let Some(e) = drawn.error {
            return Err(e);
        }
        let bitmap = drawn.bitmap;
        written.boxed += bitmap.boxed_characters;
        for (total, n) in written.causes.iter_mut().zip(bitmap.boxed_causes) {
            *total += n;
        }
        if u64::from(bitmap.width) != w || u64::from(bitmap.height) != rows {
            return Err(Error::Invalid("the page was drawn at a size other than the one asked for".to_string()));
        }
        match &mut out {
            Sink::Png(writer) => writer.rows(&bitmap.rgba)?,
            Sink::Jpeg(writer) => {
                let rgb: Vec<u8> = bitmap.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
                writer.band(&rgb)?;
            }
        }
        y += rows;
    }
    let sink = match out {
        Sink::Png(writer) => writer.finish()?,
        Sink::Jpeg(writer) => writer.finish()?,
    };
    Ok((sink, written))
}

/// Draw `page` and write it as the file `path`.
#[inline(never)]
pub fn write_page_file(renderer: &mut Renderer<'_>, page: &Page, dpi: f64, format: ImageFormat, band_pixels: u64, path: &Path) -> Result<Written> {
    let mut written = Written::default();
    write_file(path, |file| {
        let (file, got) = write_page(renderer, page, dpi, format, band_pixels, file)?;
        written = got;
        Ok(file)
    })?;
    Ok(written)
}

/// A picture of a page, ready to be a file.
pub struct Picture {
    pub ext: &'static str,
    pub data: Vec<u8>,
}

/// The names of the filters of an image stream, in order.
fn filters(dict: &crate::object::Dict) -> Vec<Vec<u8>> {
    match dict.get("Filter") {
        Some(Object::Name(n)) => vec![n.as_bytes().to_vec()],
        Some(Object::Array(a)) => a.iter().filter_map(|f| if let Object::Name(n) = f { Some(n.as_bytes().to_vec()) } else { None }).collect(),
        _ => Vec::new(),
    }
}

/// The signature box that starts a JP2 file (a bare JPEG 2000 code stream starts with the marker SOC instead).
const JP2_SIGNATURE: [u8; 8] = [0, 0, 0, 12, b'j', b'P', b' ', b' '];

/// Is `/Decode` of the picture the default one (`[0 1]` for each colour component, or none)? Anything else turns the colours, and a file with the
/// stream as it is would not show what the page shows.
fn default_decode(doc: &Document, dict: &Dict) -> bool {
    let Some(Object::Array(values)) = dict.get("Decode").and_then(|d| doc.resolve(d).ok()) else { return true };
    let number = |v: &Object| doc.resolve(v).ok().and_then(|o| o.as_f64());
    values.len() % 2 == 0 && values.as_chunks::<2>().0.iter().all(|[low, high]| number(low) == Some(0.0) && number(high) == Some(1.0))
}

/// The extension of the file a picture is saved as when its stream can be saved as it is: a JPEG or JPEG 2000 stream that no mask (`/SMask`, `/Mask`)
/// and no `/Decode` changes. A masked JPEG (a logo with a transparent ground) is not saved this way: the file would show it opaque.
fn as_is(doc: &Document, stream: &Stream) -> Option<&'static str> {
    let ext = match filters(&stream.dict).as_slice() {
        [f] if f == b"DCTDecode" && stream.data.starts_with(&[0xFF, 0xD8]) => "jpg",
        [f] if f == b"JPXDecode" && !stream.data.is_empty() => {
            if stream.data.starts_with(&JP2_SIGNATURE) {
                "jp2"
            } else {
                "j2k"
            }
        }
        _ => return None,
    };
    let masked = ["SMask", "Mask"].iter().any(|key| stream.dict.get(key).is_some_and(|v| matches!(doc.resolve(v), Ok(o) if o != Object::Null)));
    (!masked && default_decode(doc, &stream.dict)).then_some(ext)
}

/// The extension of the file the picture image XObject `obj` is saved as by [`picture_of`] (without decoding it): `jpg`, `jp2` or `j2k`, or `png`.
#[inline(never)]
pub fn picture_ext(doc: &Document, obj: ObjRef) -> &'static str {
    match doc.get(obj) {
        Ok(Object::Stream(stream)) => as_is(doc, &stream).unwrap_or("png"),
        _ => "png",
    }
}

/// The picture image XObject `obj` is, as a file: a JPEG or JPEG 2000 stream as it is (when no mask and no `/Decode` goes with it), anything else
/// decoded to a PNG, with the mask as its alpha channel. A picture that is decoded may have `max_pixels` pixels at the most (more is `Error::Limit`).
/// `Ok(None)`: it cannot be decoded. `Error::Limit` (a picture that asks for too much) and `Cancelled` are errors.
#[inline(never)]
pub fn picture_of(doc: &Document, obj: ObjRef, work: &Work, max_pixels: u64) -> Result<Option<Picture>> {
    let Object::Stream(stream) = doc.get(obj)? else { return Ok(None) };
    if let Some(ext) = as_is(doc, &stream) {
        return Ok(Some(Picture { ext, data: stream.data }));
    }
    let side = |key: &str| doc.resolve(stream.dict.get(key).unwrap_or(&Object::Null)).ok().and_then(|o| o.as_int()).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
    let (w, h) = (side("Width"), side("Height"));
    if w == 0 || h == 0 {
        return Ok(None);
    }
    if (w as u64).saturating_mul(h as u64) > max_pixels {
        return Err(Error::Limit(format!("the picture is {w} by {h} pixels, more than can be saved as a PNG here (the command line does: qingpdf images)")));
    }
    Ok(decode_image(doc, &stream, work, (w, h))?.map(|px| Picture { ext: "png", data: png(px.width, px.height, px.channels, &px.data) }))
}

/// The image XObjects `page` shows, once each, in the order it draws them.
#[inline(never)]
pub fn page_pictures(extractor: &mut TextExtractor<'_>, page: &Page, work: &Work) -> Result<Vec<ObjRef>> {
    let layout = extractor.page_layout(page, work)?;
    let mut seen: IdSet<u32> = IdSet::new();
    Ok(layout.images.iter().map(|i| i.obj).filter(|r| seen.insert(r.num)).collect())
}

/// The most a JPEG written here may be on a side (the encoder's own limit), for the dialog's sake.
pub const MAX_JPEG_SIDE: usize = jpeg::MAX_SIDE;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    #[test]
    fn names_are_the_pdfs_name_without_the_extension_and_safe_for_a_folder() {
        assert_eq!(file_stem("report.pdf"), "report");
        assert_eq!(file_stem("C:\\Users\\me\\Report 2026.PDF"), "Report 2026");
        assert_eq!(file_stem("/home/me/a.b.pdf"), "a.b");
        assert_eq!(file_stem("noext"), "noext");
        assert_eq!(file_stem(".pdf"), ".pdf");
        assert_eq!(file_stem("a:b*c?.pdf"), "a_b_c_");
        assert_eq!(file_stem("..pdf"), "page");
        assert_eq!(file_stem(""), "page");
        assert_eq!(file_stem("   .pdf"), "page");
        assert_eq!(file_stem(&"x".repeat(300)).chars().count(), 120);
        assert_eq!(file_stem("\u{4E2D}\u{6587}\u{62A5}\u{544A}.pdf"), "\u{4E2D}\u{6587}\u{62A5}\u{544A}");
        assert_eq!(page_file_name("report", 2, ImageFormat::Png), "report-3.png");
        assert_eq!(page_file_name("report", 11, ImageFormat::Jpeg(80)), "report-12.jpg");
        assert_eq!(picture_file_name("report", 2, 1, "jpg"), "report-p3-1.jpg");
    }

    fn pdf_with(objects: &[(u32, Vec<u8>)], content: &str) -> Vec<u8> {
        pdf_sized(objects, content, "0 0 200 100")
    }

    fn pdf_sized(objects: &[(u32, Vec<u8>)], content: &str, media_box: &str) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [{media_box}] /Contents 4 0 R /Resources << /XObject << /A 5 0 R /B 6 0 R /C 7 0 R >> >> >>"));
        b.stream_obj(4, "", content.as_bytes());
        for (n, bytes) in objects {
            b.raw_obj(*n, bytes);
        }
        b.finish_classic(20, "/Root 1 0 R")
    }

    fn image_object(n: u32, dict: &str, data: &[u8]) -> (u32, Vec<u8>) {
        let mut v = format!("{n} 0 obj\n<< /Type /XObject /Subtype /Image {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream\nendobj\n");
        (n, v)
    }

    #[test]
    fn a_page_in_bands_is_the_page_at_once_and_a_jpeg_is_close() {
        let content = "1 0 0 rg 10 10 80 40 re f 0 0 1 rg 100 20 90 70 re f 0 g 5 5 190 90 re S";
        let data = pdf_with(&[], content);
        let doc = Document::from_bytes(data).unwrap();
        let page = doc.pages().unwrap().remove(0);
        let mut r = Renderer::new(&doc, Box::new(crate::render::NoWidgets));
        let whole = r.render_page(&page, 150.0).unwrap();
        let rgb: Vec<u8> = whole.rgba.as_chunks::<4>().0.iter().flat_map(|p| p.iter().take(3).copied()).collect();
        // PNG: bands of 7 rows, then one band for all. The picture decoded is the whole page either way.
        for band in [7 * 417, 1 << 30] {
            let (bytes, size) = write_page(&mut r, &page, 150.0, ImageFormat::Png, band, Vec::new()).unwrap();
            assert_eq!((size.width, size.height), (whole.width, whole.height));
            let mut reader = ::png::Decoder::new(std::io::Cursor::new(&bytes)).read_info().unwrap();
            let mut buf = vec![0; reader.output_buffer_size().unwrap()];
            reader.next_frame(&mut buf).unwrap();
            assert_eq!(buf, rgb, "band {band}");
        }
        // JPEG: close to the page (a few levels), bands of a multiple of 16 rows.
        let (jpeg_bytes, _) = write_page(&mut r, &page, 150.0, ImageFormat::Jpeg(95), 20 * 417, Vec::new()).unwrap();
        let (jw, jh, back) = crate::render::image::jpeg_samples(&jpeg_bytes, 3, usize::MAX).unwrap();
        assert_eq!((jw, jh), (whole.width as usize, whole.height as usize));
        let off = back.iter().zip(&rgb).filter(|(a, b)| a.abs_diff(**b) > 40).count();
        assert!(off * 50 < rgb.len(), "{off} of {} samples are far off", rgb.len());
        // A page too big for the limit is refused, not drawn.
        let huge = Document::from_bytes(pdf_sized(&[], "", "0 0 9000 9000")).unwrap();
        let huge_page = huge.pages().unwrap().remove(0);
        let mut r2 = Renderer::new(&huge, Box::new(crate::render::NoWidgets));
        assert!(matches!(write_page(&mut r2, &huge_page, 600.0, ImageFormat::Png, 1 << 22, Vec::new()), Err(Error::Limit(_))));
    }

    #[test]
    fn pictures_are_saved_as_they_are_or_decoded_to_a_png_with_the_mask_as_alpha() {
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).unwrap();
        let raw: Vec<u8> = (0..4 * 4 * 3).map(|i| (i * 5) as u8).collect();
        let mask: Vec<u8> = (0..16).map(|i| (i * 16) as u8).collect();
        let objects = vec![
            image_object(5, "/Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", &jpeg),
            image_object(6, "/Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceRGB /SMask 8 0 R", &raw),
            image_object(8, "/Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceGray", &mask),
            image_object(7, "/Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceRGB", &raw),
        ];
        let doc = Document::from_bytes(pdf_with(&objects, "q 50 0 0 50 0 0 cm /A Do Q q 50 0 0 50 60 0 cm /B Do Q q 50 0 0 50 120 0 cm /C Do Q q 50 0 0 50 120 50 cm /C Do Q")).unwrap();
        let page = doc.pages().unwrap().remove(0);
        let work = Work::new();
        let mut extractor = TextExtractor::new(&doc);
        let found = page_pictures(&mut extractor, &page, &work).unwrap();
        // (Image C is drawn twice and is one.)
        assert_eq!(found.iter().map(|r| r.num).collect::<Vec<_>>(), vec![5, 6, 7]);
        // The JPEG as it is, byte for byte.
        let a = picture_of(&doc, found[0], &work, u64::MAX).unwrap().unwrap();
        assert_eq!((a.ext, a.data == jpeg), ("jpg", true));
        // The one with a soft mask: a PNG with an alpha channel that is the mask.
        let b = picture_of(&doc, found[1], &work, u64::MAX).unwrap().unwrap();
        assert_eq!(b.ext, "png");
        let mut reader = ::png::Decoder::new(std::io::Cursor::new(&b.data)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height, info.color_type), (4, 4, ::png::ColorType::Rgba));
        let alpha: Vec<u8> = buf.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        assert_eq!(alpha, mask);
        // The plain one: RGB, the samples as they are.
        let c = picture_of(&doc, found[2], &work, u64::MAX).unwrap().unwrap();
        let mut reader = ::png::Decoder::new(std::io::Cursor::new(&c.data)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf, raw);
        // Something that is not a picture, or a picture without a size, is nothing.
        assert!(picture_of(&doc, ObjRef::new(4, 0), &work, u64::MAX).unwrap().is_none());
        assert!(picture_of(&doc, ObjRef::new(99, 0), &work, u64::MAX).unwrap().is_none());
    }

    fn decoded_png(data: &[u8]) -> (::png::ColorType, Vec<u8>) {
        let mut reader = ::png::Decoder::new(std::io::Cursor::new(data)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        (info.color_type, buf)
    }

    #[test]
    fn a_picture_with_a_mask_or_a_decode_array_is_decoded_and_one_without_is_saved_as_it_is() {
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).unwrap();
        let mask: Vec<u8> = (0..64 * 64).map(|i| (i % 256) as u8).collect();
        let jpeg_image = |n: u32, extra: &str| image_object(n, &format!("/Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode {extra}"), &jpeg);
        let objects = vec![
            jpeg_image(5, "/SMask 8 0 R"),
            jpeg_image(6, "/Decode [1 0 1 0 1 0]"),
            jpeg_image(7, "/Decode [0 1 0 1 0 1]"),
            image_object(8, "/Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceGray", &mask),
            jpeg_image(9, "/Mask [0 100 0 100 0 100]"),
            jpeg_image(10, "/SMask null"),
            jpeg_image(11, ""),
        ];
        let doc = Document::from_bytes(pdf_with(&objects, "")).unwrap();
        let work = Work::new();
        // A logo with a soft mask is a PNG with the mask as its alpha channel, not the opaque JPEG; so is one whose colours are turned or that has a
        // colour key mask. A /Decode that changes nothing, no mask at all and a mask that is `null` leave the stream as it is.
        for (num, ext) in [(5, "png"), (6, "png"), (7, "jpg"), (9, "png"), (10, "jpg"), (11, "jpg")] {
            let got = picture_of(&doc, ObjRef::new(num, 0), &work, u64::MAX).unwrap().unwrap();
            assert_eq!((got.ext, picture_ext(&doc, ObjRef::new(num, 0))), (ext, ext), "object {num}");
            assert_eq!(got.data == jpeg, ext == "jpg", "object {num}");
        }
        let masked = picture_of(&doc, ObjRef::new(5, 0), &work, u64::MAX).unwrap().unwrap();
        let (kind, buf) = decoded_png(&masked.data);
        assert_eq!(kind, ::png::ColorType::Rgba);
        let alpha: Vec<u8> = buf.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        assert_eq!((alpha.len(), alpha.iter().min(), alpha.iter().max()), (64 * 64, Some(&0), Some(&255)));
        // The turned colours are the other ones.
        let turned = decoded_png(&picture_of(&doc, ObjRef::new(6, 0), &work, u64::MAX).unwrap().unwrap().data).1;
        let plain = decoded_png(&picture_of(&doc, ObjRef::new(9, 0), &work, u64::MAX).unwrap().unwrap().data).1;
        assert_ne!(turned, plain);
        // A picture that is not a stream is not a picture of any kind.
        assert_eq!(picture_ext(&doc, ObjRef::new(4, 0)), "png");
        assert_eq!(picture_ext(&doc, ObjRef::new(99, 0)), "png");
    }

    #[test]
    fn a_picture_that_is_decoded_has_a_most_and_one_that_is_saved_as_it_is_has_none() {
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).unwrap();
        let objects = vec![
            image_object(5, "/Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceRGB", &[7; 4 * 4 * 3]),
            image_object(6, "/Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", &jpeg),
            // (Eight thousand pixels a side with next to no data: it is the size that is looked at, before anything is allocated.)
            image_object(7, "/Width 8000 /Height 8000 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[0; 16]),
        ];
        let doc = Document::from_bytes(pdf_with(&objects, "")).unwrap();
        let work = Work::new();
        assert!(picture_of(&doc, ObjRef::new(5, 0), &work, 16).unwrap().is_some());
        assert!(matches!(picture_of(&doc, ObjRef::new(5, 0), &work, 15), Err(Error::Limit(_))));
        assert!(picture_of(&doc, ObjRef::new(6, 0), &work, 1).unwrap().is_some_and(|p| p.ext == "jpg"));
        match picture_of(&doc, ObjRef::new(7, 0), &work, READER_PICTURE_PIXELS) {
            Err(Error::Limit(why)) => assert!(why.contains("8000 by 8000"), "{why}"),
            other => panic!("{:?}", other.map(|p| p.map(|p| p.ext))),
        }
    }

    #[test]
    fn a_new_file_never_takes_the_place_of_one_that_is_there() {
        let dir = std::env::temp_dir().join(format!("qingpdf-export-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p-1.jpg"), b"old").unwrap();
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name(&write_new_file(&dir, "p-1", "jpg", b"new one").unwrap()), "p-1-2.jpg");
        assert_eq!(name(&write_new_file(&dir, "p-1", "jpg", b"new two").unwrap()), "p-1-3.jpg");
        assert_eq!(name(&write_new_file(&dir, "q", "png", b"fresh").unwrap()), "q.png");
        assert_eq!(std::fs::read(dir.join("p-1.jpg")).unwrap(), b"old");
        assert_eq!(std::fs::read(dir.join("p-1-2.jpg")).unwrap(), b"new one");
        assert_eq!(std::fs::read(dir.join("p-1-3.jpg")).unwrap(), b"new two");
        // A folder of that name is a name taken.
        std::fs::create_dir(dir.join("r.jpg")).unwrap();
        assert_eq!(name(&write_new_file(&dir, "r", "jpg", b"x").unwrap()), "r-2.jpg");
        // No folder, no file; and nothing but the files that were written is in the folder.
        assert!(write_new_file(&dir.join("missing"), "x", "jpg", b"x").is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_jpeg_2000_stream_is_saved_as_a_jp2_or_a_j2k_by_what_it_starts_with() {
        let jp2 = [0u8, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 0x87, 10, 1, 2, 3];
        let raw_stream = [0xFFu8, 0x4F, 0xFF, 0x51, 1, 2, 3];
        let objects = vec![image_object(5, "/Width 4 /Height 4 /Filter /JPXDecode", &jp2), image_object(6, "/Width 4 /Height 4 /Filter /JPXDecode", &raw_stream)];
        let doc = Document::from_bytes(pdf_with(&objects, "")).unwrap();
        let work = Work::new();
        let a = picture_of(&doc, ObjRef::new(5, 0), &work, u64::MAX).unwrap().unwrap();
        let b = picture_of(&doc, ObjRef::new(6, 0), &work, u64::MAX).unwrap().unwrap();
        assert_eq!((a.ext, a.data.as_slice()), ("jp2", &jp2[..]));
        assert_eq!((b.ext, b.data.as_slice()), ("j2k", &raw_stream[..]));
    }

    #[test]
    fn a_file_is_written_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("qingpdf-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.bin");
        write_file(&path, |mut f| {
            f.write_all(b"hello")?;
            Ok(f)
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        // A failure part-way leaves the file that was there as it was, and no temporary file.
        let failed = write_file(&path, |mut f| {
            f.write_all(b"new data")?;
            Err(Error::Cancelled)
        });
        assert!(matches!(failed, Err(Error::Cancelled)));
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
