//! Rendering (layer 3): a page drawn to pixels. Paths, colours, clipping, images, form XObjects
//! and text (embedded or system fonts) are drawn; a character whose glyph cannot be had is an outline
//! box. Transparency (groups, soft masks, blend modes, knockout), shadings (types 1 to 7), tiling and
//! shading patterns, and optional content (layers that are off) are drawn too (3c), and so are the JBIG2 and
//! JPEG 2000 image formats (3c2), and the appearance streams of annotations (3c2-3; [`Renderer::set_annotations`]
//! turns them off).
//!
//! ```no_run
//! # use qingpdf_core::{Document, render};
//! let doc = Document::open("a.pdf")?;
//! let mut renderer = render::Renderer::new(&doc);
//! let page = &doc.pages()?[0];
//! let bitmap = renderer.render_page(page, 150.0)?;
//! std::fs::write("page.png", bitmap.to_png()?)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// Pixel buffers are walked four bytes at a time all over this module; `as_chunks` would not read better.
#![allow(clippy::chunks_exact_to_as_chunks)]

mod ccitt;
mod cff;
mod color;
mod fonts;
mod func;
mod glyf;
mod image;
mod interp;
mod jbig2;
mod jpx;
mod mq;
mod oc;
mod outline;
mod shading;
mod sysfont;
mod ttvm;
mod type1;
pub(crate) mod work;

#[cfg(test)]
mod annot_tests;
#[cfg(test)]
mod font_tests;
#[cfg(test)]
mod hostile_tests;
#[cfg(test)]
mod transparency_tests;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tiny_skia::{Color, Pixmap};

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::text;

use func::Meter;
use interp::{Interp, Resources, Shared};

/// Most pixels a rendered page may have (64 MB of pixels; the memory goal is 200 MB in all).
pub const MAX_PAGE_PIXELS: u64 = 16_000_000;
/// Steps a page may spend running the tint functions of Separation and DeviceN colours (a table lookup
/// is one, an operator of a PostScript function is one). Past it those colours are shown as greys.
const MAX_FUNCTION_STEPS: u64 = 50_000_000;
/// The dpi range accepted.
pub const MIN_DPI: f64 = 1.0;
pub const MAX_DPI: f64 = 2400.0;

/// A rendered page: 8-bit RGBA, rows from the top, opaque (the page is on white paper).
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Characters that were drawn as an outline box because their glyph could not be had.
    pub boxed_characters: usize,
    /// Of those: the font has no program and no system font stands in for it; the encoding does not say
    /// what character the code is; the font has no glyph for the character.
    pub boxed_causes: [usize; 3],
    /// Characters whose glyph the font does not have; nothing is drawn for them.
    pub absent_glyphs: usize,
}

impl Bitmap {
    /// The page as a PNG file (RGB; there is no transparency to keep).
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let rgb: Vec<u8> = self.rgba.chunks_exact(4).flat_map(|p| p.iter().take(3).copied()).collect();
        let mut out = Vec::with_capacity(rgb.len() / 4 + 1024);
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| Error::Invalid(format!("cannot write the PNG: {e}")))?;
        writer.write_image_data(&rgb).map_err(|e| Error::Invalid(format!("cannot write the PNG: {e}")))?;
        writer.finish().map_err(|e| Error::Invalid(format!("cannot write the PNG: {e}")))?;
        Ok(out)
    }
}

/// What [`Renderer::render_partial`] drew: the page, and what stopped it before the end, if anything.
pub struct Partial {
    pub bitmap: Bitmap,
    /// The error that stopped the content stream part-way (a limit, or a stream that cannot be read); the bitmap
    /// has what was drawn before it.
    pub error: Option<Error>,
    /// The page used up its allowance of work and the rest of it is not drawn.
    pub work_over: bool,
}

/// Limits on the memory a [`Renderer`] holds besides the page itself, for a program that must stay within a total
/// (see [`Renderer::set_memory_limits`]). The page drawn is `width * height * 4` bytes on top of this.
#[derive(Clone, Copy, Debug)]
pub struct MemoryLimits {
    /// Bytes kept from page to page: parsed forms, shadings, patterns, decoded images, font programs, glyphs.
    pub caches: usize,
    /// Bytes of off-screen layers (transparency groups, soft masks, pattern cells) alive at once.
    pub layers: usize,
    /// Bytes of clip masks alive at once.
    pub masks: usize,
    /// Bytes the JPEG 2000 and JBIG2 decoders may hold at once.
    pub decoders: usize,
    /// Bytes for one large image (one that does not fit the image cache) kept from request to request, so that the pieces
    /// of one page drawn one after another decode it once. 0: none is kept.
    pub image_slot: usize,
    /// The most pixels one piece may have (never more than [`MAX_PAGE_PIXELS`]); a bigger request is an [`Error::Limit`].
    pub canvas_pixels: u64,
}

/// Draws the pages of one document. Fonts, images and forms are kept between pages, so one
/// renderer should do all the pages of a run.
pub struct Renderer<'a> {
    doc: &'a Document,
    shared: Shared,
    annotations: bool,
}

impl<'a> Renderer<'a> {
    pub fn new(doc: &'a Document) -> Renderer<'a> {
        Renderer { doc, shared: Shared::default(), annotations: true }
    }

    /// Draw the annotations' appearances (12.5) on top of the page's content, or not. They are drawn by default.
    pub fn set_annotations(&mut self, on: bool) {
        self.annotations = on;
    }

    /// Draw one page at `dpi` dots per inch (a page is 72 user units to the inch). The visible part
    /// is the crop box inside the media box, turned by `/Rotate`.
    ///
    /// Errors: [`Error::PasswordRequired`] for a locked file; [`Error::Invalid`] for a dpi outside
    /// 1 to 2400; [`Error::Limit`] when the page would have more than [`MAX_PAGE_PIXELS`] pixels or asks
    /// for more work than the limits allow (operators, path segments, painted area, image pixels,
    /// the document's decoding budget). Whatever else is wrong in the page (an image or font that cannot be
    /// read, an unknown colour space) is skipped and reported by [`Renderer::take_warnings`].
    pub fn render_page(&mut self, page: &Page, dpi: f64) -> Result<Bitmap> {
        let drawn = self.render_partial(page, dpi, 0, None)?;
        match drawn.error {
            Some(e) => Err(e),
            None => Ok(drawn.bitmap),
        }
    }

    /// The size in pixels of the whole page as [`Renderer::render_partial`] draws it at `dpi`, turned a further `rotation`
    /// degrees (at least 1 by 1).
    pub fn page_pixels(page: &Page, dpi: f64, rotation: u16) -> (u64, u64) {
        let [x0, y0, x1, y1] = text::visible_box(page).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let rotate = (page.rotate().rem_euclid(360) + i64::from(rotation)).rem_euclid(360);
        let scale = dpi / 72.0;
        let (bw, bh) = ((x1 - x0) * scale, (y1 - y0) * scale);
        let (pw, ph) = if rotate == 90 || rotate == 270 { (bh, bw) } else { (bw, bh) };
        ((pw.round().max(1.0)) as u64, (ph.round().max(1.0)) as u64)
    }

    /// [`Renderer::render_page`] for a reader: the page turned a further `rotation` degrees (a multiple of 90), and
    /// only the `region` of it drawn, `[x, y, width, height]` in pixels of the whole page at `dpi` (cut to the page;
    /// the region may have [`MAX_PAGE_PIXELS`] pixels although the whole page has more). A page that goes wrong
    /// part-way is not an error here: what was drawn is returned with the error (and a page that used up its
    /// allowance of work with `work_over`). [`Error::Cancelled`] when the flag of [`Renderer::set_cancel`] was set.
    pub fn render_partial(&mut self, page: &Page, dpi: f64, rotation: u16, region: Option<[u32; 4]>) -> Result<Partial> {
        if self.doc.is_locked() {
            return Err(Error::PasswordRequired);
        }
        if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
            return Err(Error::Invalid(format!("the resolution must be between {MIN_DPI} and {MAX_DPI} dpi")));
        }
        if !rotation.is_multiple_of(90) {
            return Err(Error::Invalid("the rotation must be a multiple of 90 degrees".to_string()));
        }
        // 14.11.2: the page is its crop box; without usable boxes, US Letter.
        let [x0, y0, x1, y1] = text::visible_box(page).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let rotate = (page.rotate().rem_euclid(360) + i64::from(rotation)).rem_euclid(360);
        let scale = dpi / 72.0;
        let (full_w, full_h) = Renderer::page_pixels(page, dpi, rotation);
        // The part to draw: all of the page, or the region cut to it.
        let (rx, ry, width, height) = match region {
            None => (0, 0, full_w, full_h),
            Some([x, y, w, h]) => {
                let (x, y) = (u64::from(x).min(full_w), u64::from(y).min(full_h));
                let (w, h) = (u64::from(w).min(full_w - x), u64::from(h).min(full_h - y));
                if w == 0 || h == 0 {
                    return Err(Error::Invalid("the region is outside the page".to_string()));
                }
                (x, y, w, h)
            }
        };
        let most = MAX_PAGE_PIXELS.min(self.shared.caps.canvas);
        if width.saturating_mul(height) > most {
            return Err(Error::Limit(format!(
                "the page would be {width} by {height} pixels at {dpi} dpi; the most is {most} pixels (use a lower --dpi)"
            )));
        }
        let mut pixmap = Pixmap::new(width as u32, height as u32).ok_or_else(|| Error::Limit("the page is too large to draw".to_string()))?;
        pixmap.fill(Color::WHITE);
        // User space to device pixels (row vector convention, 8.3.4): turn, scale, flip.
        let s = scale;
        let mut base = match rotate {
            90 => [0.0, s, s, 0.0, -y0 * s, -x0 * s],
            180 => [-s, 0.0, 0.0, s, x1 * s, -y0 * s],
            270 => [0.0, -s, -s, 0.0, y1 * s, x1 * s],
            _ => [s, 0.0, 0.0, -s, -x0 * s, y1 * s],
        };
        // The region's corner is the origin of the pixmap.
        base[4] -= rx as f64;
        base[5] -= ry as f64;
        // Every page has the decoding budget of a whole document (one for all of a long scan's pages would run out).
        let _budget = self.doc.page_decode_budget();
        let content = text::page_content(self.doc, page, &mut self.shared.warnings)?;
        let resources = Resources::new(self.doc, page.resources());
        let meter = Meter::new(MAX_FUNCTION_STEPS);
        let annotations = self.annotations;
        let mut interp = Interp::new(self.doc, &mut self.shared, pixmap, base, meter.clone());
        let result = interp.run(&content, &resources, 0);
        if annotations && result.is_ok() {
            interp.draw_annotations(page, &resources);
        }
        let boxed_characters = interp.boxed;
        let boxed_causes = interp.boxed_by;
        let absent_glyphs = interp.absent;
        let work_over = interp.work_over();
        let cancelled = interp.work_cancelled();
        #[cfg(test)]
        let last_work = interp.work_used();
        let pixmap = interp.pixmap;
        #[cfg(test)]
        {
            self.shared.last_work = last_work;
        }
        if work_over {
            self.shared
                .warnings
                .add("the page asks for more work (transparency, shadings, patterns, optional content) than is allowed; the rest of it is not drawn");
        }
        if meter.was_refused() {
            self.shared.warnings.add("the page asks for more colour conversion work than is allowed; some spot colours are shown as greys");
        }
        // Stopped by the flag, either between two operators or in the middle of a decoder (which then reports a limit).
        if matches!(result, Err(Error::Cancelled)) || cancelled {
            return Err(Error::Cancelled);
        }
        let bitmap = Bitmap { width: pixmap.width(), height: pixmap.height(), rgba: pixmap.take(), boxed_characters, boxed_causes, absent_glyphs };
        Ok(Partial { bitmap, error: result.err(), work_over })
    }

    /// Stop the page being drawn when `flag` is set (from another thread): [`Renderer::render_partial`] and
    /// [`Renderer::render_page`] then return [`Error::Cancelled`] at the next operator of the content stream.
    pub fn set_cancel(&mut self, flag: Option<Arc<AtomicBool>>) {
        self.shared.cancel = flag;
    }

    /// Hold the caches and the work memory of this renderer to these limits (never more than the defaults). The
    /// page being drawn is not counted: it is `width * height * 4` bytes.
    pub fn set_memory_limits(&mut self, limits: MemoryLimits) {
        self.shared.set_caps(interp::Caps::within(&limits));
    }

    #[cfg(test)]
    pub(crate) fn cache_bytes_for_test(&self) -> (usize, usize, usize) {
        self.shared.cache_bytes()
    }

    #[cfg(test)]
    pub(crate) fn bitmap_peak_for_test(&self) -> usize {
        self.shared.bitmap_peak()
    }

    #[cfg(test)]
    pub(crate) fn font_entries_for_test(&self) -> usize {
        self.shared.font_entries()
    }

    /// The work meter's units the last page drawn used.
    #[cfg(test)]
    pub(crate) fn last_work_for_test(&self) -> f64 {
        self.shared.last_work
    }

    /// (bytes held in forms, shadings and patterns, patterns kept, shadings kept).
    #[cfg(test)]
    pub(crate) fn object_cache_for_test(&self) -> (usize, usize, usize) {
        self.shared.object_cache()
    }

    /// What went wrong without stopping the rendering so far (each kind once, at most 50), and forget it.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.shared.warnings.list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    /// A one-page document, 100 by 100 points: object 3 is the page with these resources and extra
    /// page entries, 4 its content; `objs` and `streams` are further objects.
    pub(super) fn page_doc(page_extra: &str, resources: &str, content: &[u8], objs: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Document {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources {resources} /Contents 4 0 R {page_extra} >>"));
        b.stream_obj(4, "", content);
        let mut top = 4;
        for (n, body) in objs {
            b.obj(*n, body);
            top = top.max(*n);
        }
        for (n, dict, data) in streams {
            b.stream_obj(*n, dict, data);
            top = top.max(*n);
        }
        Document::from_bytes(b.finish_classic(top + 1, "/Root 1 0 R")).expect("the test file opens")
    }

    pub(super) fn draw(doc: &Document) -> (Result<Bitmap>, Vec<String>) {
        let mut r = Renderer::new(doc);
        let pages = doc.pages().expect("pages");
        let bitmap = r.render_page(&pages[0], 72.0);
        (bitmap, r.take_warnings())
    }

    pub(super) fn pixel(b: &Bitmap, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * b.width + x) * 4) as usize;
        [b.rgba[i], b.rgba[i + 1], b.rgba[i + 2]]
    }

    fn simple(content: &str) -> Bitmap {
        let (bitmap, warnings) = draw(&page_doc("", "<< >>", content.as_bytes(), &[], &[]));
        assert!(warnings.is_empty(), "{warnings:?}");
        bitmap.expect("renders")
    }

    pub(super) const WHITE: [u8; 3] = [255, 255, 255];

    #[test]
    fn fill_and_page_geometry() {
        let b = simple("1 0 0 rg 10 10 30 30 re f");
        assert_eq!((b.width, b.height), (100, 100));
        // User space has y up: the square is at the bottom left of the picture.
        assert_eq!(pixel(&b, 20, 80), [255, 0, 0]);
        assert_eq!(pixel(&b, 20, 20), WHITE);
        assert_eq!(pixel(&b, 60, 80), WHITE);
        // Even-odd leaves the hole, non-zero fills it.
        let nz = simple("0 g 10 10 80 80 re 30 30 40 40 re f");
        let eo = simple("0 g 10 10 80 80 re 30 30 40 40 re f*");
        assert_eq!(pixel(&nz, 50, 50), [0, 0, 0]);
        assert_eq!(pixel(&eo, 50, 50), WHITE);
        assert_eq!(pixel(&eo, 15, 50), [0, 0, 0]);
    }

    #[test]
    fn rotation_and_crop_box() {
        let doc = page_doc("/Rotate 90", "<< >>", b"1 0 0 rg 0 0 20 10 re f", &[], &[]);
        let b = draw(&doc).0.expect("renders");
        // Turned clockwise, the bottom left corner is at the top left; the 20 wide side is now vertical.
        assert_eq!(pixel(&b, 5, 10), [255, 0, 0]);
        assert_eq!(pixel(&b, 5, 40), WHITE);
        let doc = page_doc("/CropBox [50 0 100 50]", "<< >>", b"0 0 1 rg 60 10 10 10 re f", &[], &[]);
        let b = draw(&doc).0.expect("renders");
        assert_eq!((b.width, b.height), (50, 50));
        assert_eq!(pixel(&b, 15, 35), [0, 0, 255]);
    }

    #[test]
    fn clipping_stroking_and_dashes() {
        let b = simple("20 20 20 20 re W n 0 0 1 rg 0 0 100 100 re f");
        assert_eq!(pixel(&b, 30, 70), [0, 0, 255]);
        assert_eq!(pixel(&b, 60, 70), WHITE);
        assert_eq!(pixel(&b, 30, 40), WHITE);
        // A triangle (not a rectangle) clips through a mask.
        let b = simple("50 50 m 90 50 l 50 90 l h W n 0 0 0 rg 0 0 100 100 re f");
        assert_eq!(pixel(&b, 60, 45), [0, 0, 0]);
        assert_eq!(pixel(&b, 80, 15), WHITE);
        let b = simple("4 w 0 50 m 100 50 l S");
        assert_eq!(pixel(&b, 50, 50), [0, 0, 0]);
        assert_eq!(pixel(&b, 50, 40), WHITE);
        let b = simple("4 w [10 10] 0 d 0 50 m 100 50 l S");
        assert_eq!(pixel(&b, 5, 50), [0, 0, 0]);
        assert_eq!(pixel(&b, 15, 50), WHITE);
        // A thin line still shows (one pixel at least).
        let b = simple("0 w 0 50 m 100 50 l S");
        assert!(pixel(&b, 50, 50)[0] < 200 || pixel(&b, 50, 49)[0] < 200);
        // q and Q restore the clip.
        let b = simple("q 1 0 0 rg 0 0 10 10 re W n Q 0 1 0 rg 0 0 100 100 re f");
        assert_eq!(pixel(&b, 50, 50), [0, 255, 0]);
    }

    #[test]
    fn colour_spaces() {
        // CMYK black ink is dark.
        let b = simple("0 0 0 1 k 0 0 100 100 re f");
        assert!(pixel(&b, 50, 50).iter().all(|&v| v < 60));
        // Separation goes through its tint function; Indexed through its table.
        let res = "<< /ColorSpace << /Cs1 [/Separation /Spot /DeviceRGB 5 0 R] /Cs2 [/Indexed /DeviceRGB 1 <FF0000 0000FF>] >> >>";
        let doc = page_doc(
            "",
            res,
            b"/Cs1 cs 0.5 sc 0 0 50 100 re f /Cs2 cs 1 sc 50 0 50 100 re f",
            &[],
            &[(5, "/FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [1 0 0] /N 1", b"")],
        );
        let (b, w) = draw(&doc);
        let b = b.expect("renders");
        assert!(w.is_empty(), "{w:?}");
        let [r, g, bl] = pixel(&b, 25, 50);
        assert!(r > 250 && (120..135).contains(&g) && (120..135).contains(&bl), "{r} {g} {bl}");
        assert_eq!(pixel(&b, 75, 50), [0, 0, 255]);
    }

    #[test]
    fn inline_images_and_masks() {
        // A 2 by 2 grey image over the page, then a stencil mask (11110000) painted red over the lower half.
        let mut content = b"q 100 0 0 100 0 0 cm BI /W 2 /H 2 /BPC 8 /CS /G ID ".to_vec();
        content.extend_from_slice(&[0x00, 0xFF, 0xFF, 0x00]);
        content.extend_from_slice(b" EI Q 1 0 0 rg q 100 0 0 50 0 0 cm BI /W 8 /H 1 /IM true /BPC 1 ID ");
        content.push(0xF0);
        content.extend_from_slice(b" EI Q");
        let (b, w) = draw(&page_doc("", "<< >>", &content, &[], &[]));
        let b = b.expect("renders");
        assert!(w.is_empty(), "{w:?}");
        // The grey image: first row black, white; second row white, black (rows from the top).
        assert_eq!(pixel(&b, 20, 20), [0, 0, 0]);
        assert_eq!(pixel(&b, 80, 20), WHITE);
        // The stencil paints where the sample is 0: the right half of the lower 50 points.
        assert_eq!(pixel(&b, 20, 80), WHITE);
        assert_eq!(pixel(&b, 80, 80), [255, 0, 0]);
    }

    #[test]
    fn image_xobjects_flate_decode_and_ccitt() {
        // A 2 by 2 RGB image through /Decode [1 0 ...] (inverted).
        let data = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
        let res = "<< /XObject << /Im 5 0 R /Fax 6 0 R >> >>";
        // Group 4, 8 columns, 2 rows: "..###..." twice, 001 0111 10 1 | 1 1 1 | EOFB (the ccitt module's test).
        let fax = [0x2Fu8, 0x78, 0x00, 0x80, 0x08];
        let doc = page_doc(
            "",
            res,
            b"q 50 0 0 50 0 50 cm /Im Do Q q 50 0 0 50 50 0 cm /Fax Do Q",
            &[],
            &[
                (5, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Decode [1 0 1 0 1 0]", &data),
                (
                    6,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 2 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 8 /Rows 2 >>",
                    &fax,
                ),
            ],
        );
        let (b, w) = draw(&doc);
        let b = b.expect("renders");
        assert!(w.is_empty(), "{w:?}");
        // Top left quarter: the image's first pixel (255,0,0) inverted is (0,255,255).
        assert_eq!(pixel(&b, 10, 10), [0, 255, 255]);
        assert_eq!(pixel(&b, 40, 40), [0, 0, 255]);
        // Bottom right quarter: white for the first 2 of 8 columns, black for 3, white again.
        assert_eq!(pixel(&b, 52, 60), WHITE);
        assert_eq!(pixel(&b, 75, 60), [0, 0, 0]);
        assert_eq!(pixel(&b, 97, 60), WHITE);
    }

    #[test]
    fn jpeg_images() {
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).expect("fixture");
        let doc = page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im Do Q",
            &[],
            &[(5, "/Type /XObject /Subtype /Image /Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", &jpeg)],
        );
        let (b, w) = draw(&doc);
        let b = b.expect("renders");
        assert!(b.rgba.chunks_exact(4).any(|p| p[0] != 255 || p[1] != 255 || p[2] != 255), "{w:?}");
    }

    #[test]
    fn type3_glyphs_forms_and_boxes() {
        // A Type 3 glyph is drawn from its procedure, a form is drawn and clipped; a character the encoding names
        // with something that is no character has no glyph to draw and is a box.
        let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /sq 6 0 R >> \
                    /Encoding << /Type /Encoding /Differences [97 /sq] >> /FirstChar 97 /LastChar 97 /Widths [1000] >>";
        let doc = page_doc(
            "",
            "<< /Font << /T3 5 0 R /H 7 0 R >> /XObject << /Fm 8 0 R >> >>",
            b"BT /T3 40 Tf 10 10 Td 1 0 0 rg (aa) Tj ET q 1 0 0 1 0 50 cm /Fm Do Q BT /H 10 Tf 60 60 Td (A) Tj ET",
            &[(5, font), (7, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Differences [65 /nosuchglyphname] >> >>")],
            &[
                (6, "", b"1000 0 0 0 1000 1000 d1 100 100 800 800 re f"),
                (8, "/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Matrix [1 0 0 1 60 0]", b"0 0 1 rg 0 0 100 100 re f"),
            ],
        );
        let (b, w) = draw(&doc);
        let b = b.expect("renders");
        assert!(w.is_empty(), "{w:?}");
        // Two squares of the Type 3 glyph at 40 points: each glyph is 40 wide, the square 32 wide at +4.
        assert_eq!(pixel(&b, 30, 70), [255, 0, 0]);
        assert_eq!(pixel(&b, 70, 70), [255, 0, 0]);
        assert_eq!(pixel(&b, 8, 70), WHITE);
        // The form is clipped to its bounding box: 20 points, moved by 60 and 50.
        assert_eq!(pixel(&b, 78, 48), [0, 0, 255]);
        assert_eq!(pixel(&b, 90, 40), WHITE);
        assert_eq!(b.boxed_characters, 1);
        assert_eq!(b.boxed_causes, [0, 1, 0]);
    }

    #[test]
    fn broken_and_hostile_input() {
        // A form that contains itself, a missing resource, junk operators, a path with no start.
        let doc = page_doc(
            "",
            "<< /XObject << /Fm 5 0 R >> >>",
            b"/Fm Do /Nope Do 1 2 3 foo bar ( l 5 5 l S 0 0 m 10 10 l 1 0 0 rg",
            &[],
            &[(5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Fm 5 0 R >> >>", b"/Fm Do 0 g 0 0 10 10 re f")],
        );
        let (b, w) = draw(&doc);
        b.expect("renders");
        assert!(w.iter().any(|m| m.contains("contains itself")), "{w:?}");
        // An image that asks for far too much.
        let doc = page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im Do Q",
            &[],
            &[(5, "/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /BitsPerComponent 8 /ColorSpace /DeviceGray", b"x")],
        );
        assert!(matches!(draw(&doc).0, Err(Error::Limit(_))));
        // An image that is not one is skipped with a warning.
        let doc = page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im Do Q",
            &[],
            &[(5, "/Type /XObject /Subtype /Image /Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /DCTDecode", b"not a jpeg")],
        );
        let (b, w) = draw(&doc);
        b.expect("renders");
        assert!(w.iter().any(|m| m.contains("image is skipped")), "{w:?}");
        // Too many pixels for a page; a dpi that is not one.
        let doc = page_doc("/MediaBox [0 0 1000 1000]", "<< >>", b"", &[], &[]);
        let mut r = Renderer::new(&doc);
        let pages = doc.pages().expect("pages");
        assert!(matches!(r.render_page(&pages[0], 2400.0), Err(Error::Limit(_))));
        assert!(matches!(r.render_page(&pages[0], 0.0), Err(Error::Invalid(_))));
        assert!(matches!(r.render_page(&pages[0], f64::NAN), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_big_image_is_shrunk_by_averaging() {
        // 3 grey pixels (0, 255, 0) on 2 device pixels: each device pixel covers 1.5 of them.
        let res = "<< /XObject << /G 5 0 R /B 6 0 R >> >>";
        let mut bits = Vec::new();
        for y in 0..16u32 {
            // A one pixel checkerboard: 0101... then 1010...
            bits.extend_from_slice(if y % 2 == 0 { &[0x55u8, 0x55] } else { &[0xAAu8, 0xAA] });
        }
        let doc = page_doc(
            "",
            res,
            b"q 2 0 0 1 0 0 cm /G Do Q q 8 0 0 8 20 20 cm /B Do Q",
            &[],
            &[
                (5, "/Type /XObject /Subtype /Image /Width 3 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[0, 255, 0]),
                (6, "/Type /XObject /Subtype /Image /Width 16 /Height 16 /BitsPerComponent 1 /ColorSpace /DeviceGray", &bits),
            ],
        );
        let b = draw(&doc).0.expect("renders");
        for x in 0..2 {
            let v = pixel(&b, x, 99)[0];
            assert!((84..=86).contains(&v), "pixel {x} is {v}");
        }
        // Black and white squares of one pixel, halved: grey.
        for (x, y) in [(20, 72), (23, 75), (27, 79)] {
            let v = pixel(&b, x, y)[0];
            assert!((126..=129).contains(&v), "({x}, {y}) is {v}");
        }
    }

    #[test]
    fn short_and_mismatched_image_data_do_not_panic() {
        // Samples that stop in the middle of the image (the rest is black), a 16-bit image, a JPEG whose
        // size is not the one the dictionary gives, a colour key mask and a mask of another size.
        let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rgb_progressive.jpg")).expect("fixture");
        let doc = page_doc(
            "",
            "<< /XObject << /A 5 0 R /B 6 0 R /C 7 0 R /D 8 0 R >> >>",
            b"q 50 0 0 50 0 0 cm /A Do Q q 50 0 0 50 50 0 cm /B Do Q q 50 0 0 50 0 50 cm /C Do Q q 50 0 0 50 50 50 cm /D Do Q",
            &[],
            &[
                (5, "/Type /XObject /Subtype /Image /Width 4 /Height 4 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[200, 100, 50]),
                (6, "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 16 /ColorSpace /DeviceRGB", &[255, 255, 0, 0, 0, 0, 0, 255, 255, 255, 0, 0]),
                (7, "/Type /XObject /Subtype /Image /Width 10 /Height 10 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode", &jpeg),
                (8, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Mask [0 100] /SMask 9 0 R", &[10, 200, 150, 250]),
                (9, "/Type /XObject /Subtype /Image /Width 3 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[255, 128]),
            ],
        );
        let (b, _) = draw(&doc);
        b.expect("renders");
    }

    #[test]
    fn extra_restores_in_a_form_do_not_reach_the_callers_states() {
        // The form restores more than it saved; the page's own `q` and `Q` still pair up.
        let doc = page_doc(
            "",
            "<< /XObject << /Fm 5 0 R >> >>",
            b"1 0 0 rg q 0 0 1 rg /Fm Do Q 0 0 50 50 re f",
            &[],
            &[(5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100]", b"Q Q Q 0 1 0 rg")],
        );
        let b = draw(&doc).0.expect("renders");
        assert_eq!(pixel(&b, 25, 75), [255, 0, 0]);
    }

    #[test]
    fn huge_columns_and_huge_zoom_stay_within_memory() {
        // Rows of 2 MiB each, five million of them: refused, not allocated.
        let doc = page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im Do Q",
            &[],
            &[(
                5,
                "/Type /XObject /Subtype /Image /Width 1 /Height 5000000 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 16000000 >>",
                &[0x00, 0x10, 0x01],
            )],
        );
        assert!(matches!(draw(&doc).0, Err(Error::Limit(_))));
        // A small picture shown a hundred thousand times its size (a page-sized window of it is in view).
        let data = vec![200u8; 1000 * 1000];
        let doc = page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100000000 0 0 100000000 -50000000 -50000000 cm /Im Do Q",
            &[],
            &[(5, "/Type /XObject /Subtype /Image /Width 1000 /Height 1000 /BitsPerComponent 8 /ColorSpace /DeviceGray", &data)],
        );
        let b = draw(&doc).0.expect("renders");
        assert_eq!(pixel(&b, 50, 50), [200, 200, 200]);
    }

    #[test]
    fn operator_and_nesting_limits() {
        let mut content = Vec::new();
        for _ in 0..(interp::MAX_OPERATORS / 2 + 10) {
            content.extend_from_slice(b"q Q ");
        }
        let doc = page_doc("", "<< >>", &content, &[], &[]);
        assert!(matches!(draw(&doc).0, Err(Error::Limit(_))));
        // q nested 100000 deep does not grow without end.
        let doc = page_doc("", "<< >>", &b"q ".repeat(100_000), &[], &[]);
        assert!(draw(&doc).0.is_ok());
    }

    #[test]
    fn png_output() {
        let b = simple("1 0 0 rg 0 0 100 100 re f");
        let png = b.to_png().expect("png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().expect("header");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!((info.width, info.height), (100, 100));
        assert_eq!(&buf[..3], &[255, 0, 0]);
    }

    #[test]
    fn a_page_can_be_stopped_and_drawn_in_pieces_and_turned() {
        let doc = page_doc("", "<< >>", b"1 0 0 rg 10 10 30 30 re f 0 0 1 rg 60 60 20 20 re f", &[], &[]);
        let pages = doc.pages().expect("pages");
        let page = pages.first().expect("a page");
        let mut r = Renderer::new(&doc);
        let flag = Arc::new(AtomicBool::new(true));
        r.set_cancel(Some(flag.clone()));
        assert!(matches!(r.render_partial(page, 72.0, 0, None), Err(Error::Cancelled)));
        assert!(matches!(r.render_page(page, 72.0), Err(Error::Cancelled)));
        flag.store(false, std::sync::atomic::Ordering::Relaxed);
        // A piece of the page is the same piece of the whole page.
        let whole = r.render_partial(page, 144.0, 0, None).expect("whole").bitmap;
        let piece = r.render_partial(page, 144.0, 0, Some([30, 100, 80, 60])).expect("piece").bitmap;
        assert_eq!((whole.width, whole.height, piece.width, piece.height), (200, 200, 80, 60));
        for y in 0..60usize {
            for x in 0..80usize {
                let (a, b) = ((y * 80 + x) * 4, ((y + 100) * 200 + x + 30) * 4);
                assert_eq!(piece.rgba[a..a + 4], whole.rgba[b..b + 4], "({x}, {y})");
            }
        }
        // A region outside the page is refused, one that sticks out is cut.
        assert!(matches!(r.render_partial(page, 72.0, 0, Some([500, 0, 10, 10])), Err(Error::Invalid(_))));
        let cut = r.render_partial(page, 72.0, 0, Some([90, 90, 50, 50])).expect("cut").bitmap;
        assert_eq!((cut.width, cut.height), (10, 10));
        // Turned a further 90 degrees on a page that is not square.
        let doc = page_doc("/CropBox [0 0 100 50]", "<< >>", b"1 0 0 rg 0 0 10 10 re f", &[], &[]);
        let pages = doc.pages().expect("pages");
        let mut r = Renderer::new(&doc);
        let turned = r.render_partial(pages.first().expect("a page"), 72.0, 90, None).expect("turned").bitmap;
        assert_eq!((turned.width, turned.height), (50, 100));
    }

    #[test]
    fn memory_limits_only_ever_lower_the_defaults() {
        let d = interp::Caps::default();
        let c = interp::Caps::within(&MemoryLimits { caches: 48 << 20, layers: 16 << 20, masks: 8 << 20, decoders: 32 << 20, image_slot: 24 << 20, canvas_pixels: 4 << 20 });
        assert!(c.objects + c.images + c.programs + c.glyphs + c.glyph_bitmaps <= 48 << 20);
        assert_eq!((c.layers, c.masks, c.decoders), (16 << 20, 8 << 20, 32 << 20));
        let big = interp::Caps::within(&MemoryLimits { caches: usize::MAX, layers: usize::MAX, masks: usize::MAX, decoders: usize::MAX, image_slot: 0, canvas_pixels: u64::MAX });
        assert_eq!((big.objects, big.images, big.programs, big.glyphs, big.layers, big.masks), (d.objects, d.images, d.programs, d.glyphs, d.layers, d.masks));
        let none = interp::Caps::within(&MemoryLimits { caches: 0, layers: 0, masks: 0, decoders: 0, image_slot: 0, canvas_pixels: 0 });
        assert!(none.objects >= 1 && none.programs >= 1 && none.layers >= 1 && none.masks >= 1);
    }

    fn image_page() -> Document {
        let data: Vec<u8> = (0..64 * 64 * 3).map(|i| (i % 251) as u8).collect();
        page_doc(
            "",
            "<< /XObject << /Im 5 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im Do Q",
            &[],
            &[(5, "/Type /XObject /Subtype /Image /Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceRGB", &data)],
        )
    }

    #[test]
    fn the_pieces_of_a_page_share_one_large_image() {
        let doc = image_page();
        let pages = doc.pages().expect("pages");
        let page = pages.first().expect("a page");
        // An image cache too small for the image: with no slot every piece decodes the image again, with one it is decoded once.
        let limits = |slot| MemoryLimits { caches: 100, layers: 1 << 20, masks: 1 << 20, decoders: 1 << 20, image_slot: slot, canvas_pixels: 1 << 20 };
        let mut without = Renderer::new(&doc);
        without.set_memory_limits(limits(0));
        let mut with = Renderer::new(&doc);
        with.set_memory_limits(limits(1 << 20));
        let whole = without.render_partial(page, 144.0, 0, None).expect("whole").bitmap;
        for r in [&mut without, &mut with] {
            for region in [[0, 0, 200, 100], [0, 100, 200, 100]] {
                let piece = r.render_partial(page, 144.0, 0, Some(region)).expect("piece").bitmap;
                let at = region[1] as usize * 200 * 4;
                assert_eq!(piece.rgba[..], whole.rgba[at..at + 200 * 100 * 4]);
            }
        }
        assert_eq!(without.shared.images_decoded, 1 + 2);
        assert_eq!(with.shared.images_decoded, 1, "the second piece found the image in the slot");
    }

    #[test]
    fn a_piece_bigger_than_the_canvas_is_refused() {
        let doc = page_doc("", "<< >>", b"1 0 0 rg 0 0 50 50 re f", &[], &[]);
        let pages = doc.pages().expect("pages");
        let page = pages.first().expect("a page");
        let mut r = Renderer::new(&doc);
        r.set_memory_limits(MemoryLimits { caches: 1 << 20, layers: 1 << 20, masks: 1 << 20, decoders: 1 << 20, image_slot: 0, canvas_pixels: 100 * 100 });
        assert!(matches!(r.render_partial(page, 144.0, 0, None), Err(Error::Limit(_))));
        assert!(matches!(r.render_partial(page, 144.0, 0, Some([0, 0, 101, 100])), Err(Error::Limit(_))));
        assert_eq!(r.render_partial(page, 144.0, 0, Some([0, 0, 100, 100])).expect("fits").bitmap.width, 100);
    }
}
