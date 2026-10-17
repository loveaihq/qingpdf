//! Text boxes and image stamps (4b): `FreeText` annotations (12.5.6.6) whose text is drawn with an embedded subset font (so
//! Chinese is shown by every reader), and `Stamp` annotations (12.5.6.12) that hold a picture, a PNG with its transparency kept
//! (as a soft mask) or a JPEG as it is.
//!
//! The places are rectangles of the page as it is shown (the same points as everywhere in `edit`); the appearance is drawn in
//! the upright space of the rectangle, and its `/Matrix` carries it onto the page's user space (12.5.5, Algorithm 8.1), so that
//! the box stands upright on a page that is turned.

use std::collections::BTreeSet;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::fontsub::{self, Look};
use crate::layout::{self, Da, Face, Spec};
use super::{Ctx, annotation, finite_place, reals, text_string};
use crate::error::{Error, Result};
use crate::image::{self, Format};
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::text::Shown;

/// Most characters of a text box.
pub const MAX_FREETEXT_CHARS: usize = 4_000;
/// The sizes the font of a text box may have, in points.
pub const MIN_FONT_SIZE: f64 = 4.0;
pub const MAX_FONT_SIZE: f64 = 144.0;
/// Most pixels of a stamp's picture, and most bytes of its file.
pub const MAX_STAMP_PIXELS: u64 = 16_000_000;
pub const MAX_STAMP_FILE_BYTES: usize = 12 << 20;
/// The smallest side of a box or stamp on the page, in points.
const MIN_SIDE: f64 = 8.0;
/// What a pixel of a picture costs the work meter (decoded, split into colour and alpha, compressed).
pub(super) const PIXEL_COST: f64 = 12.0;
pub(super) const BOX_COST: f64 = 300_000.0;

/// The rectangle `[left, top, right, bottom]` of the page as shown, in order, at least [`MIN_SIDE`] a side and inside the page.
pub(super) fn normalized(shown: &Shown, rect: [f32; 4]) -> Result<[f64; 4]> {
    let [a, b, c, d] = [finite_place(rect[0])?, finite_place(rect[1])?, finite_place(rect[2])?, finite_place(rect[3])?];
    let (page_w, page_h) = shown.size();
    let (mut l, mut r) = (a.min(c), a.max(c));
    let (mut t, mut bt) = (b.min(d), b.max(d));
    if r - l < MIN_SIDE {
        r = l + MIN_SIDE;
    }
    if bt - t < MIN_SIDE {
        bt = t + MIN_SIDE;
    }
    // Held to the page: moved inside it, and cut to its size if it is bigger.
    let (w, h) = ((r - l).min(page_w), (bt - t).min(page_h));
    l = l.clamp(0.0, (page_w - w).max(0.0));
    t = t.clamp(0.0, (page_h - h).max(0.0));
    (r, bt) = (l + w, t + h);
    Ok([l, t, r, bt])
}

/// The rectangle of user space that the shown rectangle `[l, t, r, b]` becomes, and the matrix that carries a space of its own
/// (origin at the lower left of the shown rectangle, x to the right, y up, as the reader sees it) onto the page's user space.
fn frame(shown: &Shown, [l, t, r, b]: [f64; 4]) -> ([f64; 4], [f64; 6]) {
    let p0 = shown.unmap(l, b);
    let pu = shown.unmap(l + 1.0, b);
    let pv = shown.unmap(l, b - 1.0);
    let matrix = [pu.0 - p0.0, pu.1 - p0.1, pv.0 - p0.0, pv.1 - p0.1, p0.0, p0.1];
    let (w, h) = (r - l, b - t);
    let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(u, v)| (matrix[0] * u + matrix[2] * v + matrix[4], matrix[1] * u + matrix[3] * v + matrix[5]));
    let rect = corners.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |m, &(x, y)| [m[0].min(x), m[1].min(y), m[2].max(x), m[3].max(y)]);
    (rect, matrix)
}

fn add_form(ctx: &mut Ctx<'_>, w: f64, h: f64, matrix: [f64; 6], content: &str, resources: Dict) -> Result<ObjRef> {
    let r = ctx.alloc()?;
    let mut dict = Dict::new();
    dict.set("Type", Object::from("XObject"));
    dict.set("Subtype", Object::from("Form"));
    dict.set("BBox", reals(&[0.0, 0.0, w, h]));
    dict.set("Matrix", reals(&matrix));
    dict.set("Resources", Object::Dict(resources));
    dict.set("Filter", Object::from("FlateDecode"));
    ctx.objects.push((r.num, Object::Stream(Stream { dict, data: compress_to_vec_zlib(content.as_bytes(), 6) })));
    Ok(r)
}

/// The text of a text box as it is kept: line ends as `\n`, no other control characters, at most [`MAX_FREETEXT_CHARS`] characters.
pub(super) fn clean_text(text: &str) -> String {
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized_text.chars().filter(|&c| c == '\n' || !c.is_control()).take(MAX_FREETEXT_CHARS).collect()
}

/// The size a text box's text is drawn at, held to the sizes it may have.
pub(super) fn clamp_size(size: f64) -> Result<f64> {
    if size.is_finite() { Ok(size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)) } else { Err(Error::Invalid("the size of the text is not a number".to_string())) }
}

/// The colour operator of a `/DA` for an RGB colour.
pub(super) fn da_color(color: [u8; 3]) -> String {
    let n = |c: u8| super::appearance::num(f64::from(c) / 255.0);
    format!("{} {} {} rg", n(color[0]), n(color[1]), n(color[2]))
}

/// How a text box is fitted to its text: it grows downwards when the text needs more room, or it stays as it is and the text must fit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Fit {
    Grow,
    Fixed,
}

/// The appearance of a text box: `clean` (see [`clean_text`]) laid out in the rectangle `at` (`[left, top, right, bottom]` of the page as shown,
/// already held to the page when it may grow) `size` points high, in the colour `color` (an operator with its operands, `0 g`); the box grows
/// downwards when the text needs more room, unless it is `Fixed`. Returns the form and the annotation's rectangle in user space.
pub(super) fn freetext_ap(ctx: &mut Ctx<'_>, shown: &Shown, at: [f64; 4], clean: &str, size: f64, color: &str, fit: Fit) -> Result<(ObjRef, [f64; 4])> {
    if !ctx.work.charge(BOX_COST + 600.0 * clean.chars().count() as f64) {
        return Err(Error::Limit("the text box took too much work".to_string()));
    }
    let chars: BTreeSet<char> = crate::order::set_of(clean.chars().filter(|&c| c != '\n'));
    let font = fontsub::choose(&chars, Look::Sans)?;
    let embedded = fontsub::embed(&font, &chars, ctx.work)?;
    let font_ref = fontsub::add_objects(ctx, &embedded)?;

    let [l, t, r, mut b] = at;
    let (_, page_h) = shown.size();
    // Room for all the lines: the box grows downwards, as far as the page goes.
    let (asc, desc) = embedded.ascent_descent();
    let (lines, cut) = layout::wrap(&embedded, clean, size, (r - l - 4.0).max(1.0), layout::MAX_LINES);
    if cut {
        return Err(Error::Limit(format!("the text takes more than {} lines", layout::MAX_LINES)));
    }
    let needed = lines.len() as f64 * (asc - desc) * size + 4.0;
    if fit == Fit::Fixed {
        if needed > b - t + 0.01 {
            return Err(Error::Invalid("the text does not fit in its box".to_string()));
        }
    } else {
        if needed > b - t {
            b = (t + needed).min(page_h.max(t + MIN_SIDE));
        }
        b = b.max(t + MIN_SIDE);
    }
    let (w, h) = (r - l, b - t);
    let (user_rect, matrix) = frame(shown, [l, t, r, b]);

    let da = Da { font: b"F0".to_vec(), size, color: color.to_string() };
    let spec = Spec { text: clean, multiline: true, q: 0, comb: None };
    let content = layout::text_content(&embedded as &dyn Face, &spec, &da, w, h, 0.0, "F0").0;
    let mut fonts = Dict::new();
    fonts.set("F0", Object::Ref(font_ref));
    let mut resources = Dict::new();
    resources.set("Font", Object::Dict(fonts));
    let form = add_form(ctx, w, h, matrix, &content, resources)?;
    Ok((form, user_rect))
}

/// The `/DA` of a text box.
pub(super) fn da_string(size: f64, color: &str) -> Object {
    Object::String(crate::object::PdfString::literal(format!("/Helv {} Tf {color}", super::appearance::num(size)).into_bytes()))
}

/// A text box with `text` in it, `size` points high, in the rectangle `rect` (grown downwards when the text needs more room).
#[allow(clippy::too_many_arguments)]
pub(super) fn freetext(ctx: &mut Ctx<'_>, shown: &Shown, rect: [f32; 4], text: &str, size: f32, color: [u8; 3], author: &str, now: &str) -> Result<Vec<ObjRef>> {
    let size = clamp_size(f64::from(size))?;
    let clean = clean_text(text);
    if clean.trim().is_empty() {
        return Err(Error::Invalid("the text box is empty".to_string()));
    }
    let da = da_color(color);
    let (form, user_rect) = freetext_ap(ctx, shown, normalized(shown, rect)?, &clean, size, &da, Fit::Grow)?;
    let annot_ref = ctx.alloc()?;
    let mut d = annotation("FreeText", user_rect, color, author, now);
    d.remove("C");
    d.set("Contents", text_string(&clean.replace('\n', "\r")));
    d.set("DA", da_string(size, &da));
    d.set("Q", Object::Integer(0));
    d.set("IT", Object::from("FreeTextTypewriter"));
    d.set("Border", Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]));
    super::with_appearance(&mut d, form);
    ctx.objects.push((annot_ref.num, Object::Dict(d)));
    Ok(vec![annot_ref])
}

/// A picture's objects in an edit: the image XObject (with its soft mask, a PNG's transparency), how a JPEG is turned (EXIF; a PNG
/// is upright) and the size it is shown at, in pixels.
pub(super) struct Picture {
    pub image: ObjRef,
    pub orientation: u8,
    pub width: u32,
    pub height: u32,
}

/// Make the objects of the picture `bytes` (a PNG or a JPEG file), charging the meter for its pixels.
pub(super) fn picture(ctx: &mut Ctx<'_>, bytes: &[u8]) -> Result<Picture> {
    if bytes.len() > MAX_STAMP_FILE_BYTES {
        return Err(Error::Limit(format!("the picture file is bigger than {} MB", MAX_STAMP_FILE_BYTES >> 20)));
    }
    let (shown_w, shown_h, format) = image::probe(bytes)?;
    let pixels = u64::from(shown_w) * u64::from(shown_h);
    if shown_w == 0 || shown_h == 0 || pixels > MAX_STAMP_PIXELS {
        return Err(Error::Limit(format!("the picture has more than {MAX_STAMP_PIXELS} pixels")));
    }
    if !ctx.work.charge(BOX_COST + PIXEL_COST * pixels as f64) {
        return Err(Error::Limit("the picture took too much work".to_string()));
    }
    let (image, orientation) = match format {
        Format::Jpeg => {
            let info = image::parse_jpeg(bytes)?;
            let r = ctx.alloc()?;
            ctx.objects.push((r.num, Object::Stream(Stream { dict: image::jpeg_dict(&info), data: bytes.to_vec() })));
            (r, info.orientation)
        }
        Format::Png => {
            let png = image::decode_png_limited(bytes, MAX_STAMP_PIXELS)?;
            let (mut dict, mask_dict) = image::png_dicts(&png);
            dict.set("Filter", Object::from("FlateDecode"));
            if let Some(alpha) = png.alpha_flate {
                let mut mask_dict = mask_dict;
                mask_dict.set("Filter", Object::from("FlateDecode"));
                let mask = ctx.alloc()?;
                ctx.objects.push((mask.num, Object::Stream(Stream { dict: mask_dict, data: alpha })));
                dict.set("SMask", Object::Ref(mask));
            }
            let r = ctx.alloc()?;
            ctx.objects.push((r.num, Object::Stream(Stream { dict, data: png.pixels_flate })));
            (r, 1)
        }
    };
    Ok(Picture { image, orientation, width: shown_w, height: shown_h })
}

/// A stamp with the picture `bytes` (a PNG or a JPEG file), as big as fits in `rect` with the picture's shape, in the middle of it.
pub(super) fn stamp(ctx: &mut Ctx<'_>, shown: &Shown, rect: [f32; 4], bytes: &[u8], author: &str, now: &str) -> Result<Vec<ObjRef>> {
    let Picture { image: image_ref, orientation, width: shown_w, height: shown_h } = picture(ctx, bytes)?;

    // As big as fits the rectangle, with the shape of the picture, in its middle.
    let [l, t, r, b] = normalized(shown, rect)?;
    let (room_w, room_h) = (r - l, b - t);
    let scale = (room_w / f64::from(shown_w)).min(room_h / f64::from(shown_h));
    let (w, h) = ((f64::from(shown_w) * scale).max(MIN_SIDE), (f64::from(shown_h) * scale).max(MIN_SIDE));
    let (page_w, page_h) = shown.size();
    let (l, t) = ((l + (room_w - w) / 2.0).clamp(0.0, (page_w - w).max(0.0)), (t + (room_h - h) / 2.0).clamp(0.0, (page_h - h).max(0.0)));
    let (user_rect, matrix) = frame(shown, [l, t, l + w, t + h]);

    let m = image::orientation_matrix(orientation, w, h);
    let content = format!("q\n{} cm\n/Im0 Do\nQ\n", m.iter().map(|v| super::appearance::num(*v)).collect::<Vec<_>>().join(" "));
    let mut images = Dict::new();
    images.set("Im0", Object::Ref(image_ref));
    let mut resources = Dict::new();
    resources.set("XObject", Object::Dict(images));
    let form = add_form(ctx, w, h, matrix, &content, resources)?;

    let annot_ref = ctx.alloc()?;
    let mut d = annotation("Stamp", user_rect, [0, 0, 0], author, now);
    d.remove("C");
    d.set("Name", Object::from("Image"));
    d.set("Border", Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]));
    super::with_appearance(&mut d, form);
    ctx.objects.push((annot_ref.num, Object::Dict(d)));
    Ok(vec![annot_ref])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(rotate: i64) -> Shown {
        let mut b = crate::testutil::PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Rotate {rotate} >>"));
        let doc = crate::Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap();
        Shown::of(&doc.pages().unwrap()[0])
    }

    #[test]
    fn a_box_is_put_in_order_inside_the_page_and_made_big_enough() {
        let s = shown(0);
        assert_eq!(normalized(&s, [100.0, 80.0, 20.0, 40.0]).unwrap(), [20.0, 40.0, 100.0, 80.0]);
        // Too small: the least side. Off the page: moved in. Bigger than the page: cut to it.
        assert_eq!(normalized(&s, [10.0, 10.0, 11.0, 11.0]).unwrap(), [10.0, 10.0, 18.0, 18.0]);
        assert_eq!(normalized(&s, [290.0, 190.0, 340.0, 230.0]).unwrap(), [250.0, 160.0, 300.0, 200.0]);
        assert_eq!(normalized(&s, [-50.0, -50.0, 900.0, 900.0]).unwrap(), [0.0, 0.0, 300.0, 200.0]);
        assert!(normalized(&s, [f32::NAN, 0.0, 10.0, 10.0]).is_err());
    }

    #[test]
    fn the_frame_of_a_box_turns_with_the_page() {
        // Not turned: the box at (20, 40) to (100, 80) of the shown page is x 20..100, y 120..160 (200 high) of user space.
        let (rect, m) = frame(&shown(0), [20.0, 40.0, 100.0, 80.0]);
        assert_eq!(rect, [20.0, 120.0, 100.0, 160.0]);
        assert_eq!(m, [1.0, 0.0, 0.0, 1.0, 20.0, 120.0]);
        // Turned a quarter: the box is on its side in user space (40 wide, 80 high), and its own x runs up the page.
        let (rect, m) = frame(&shown(90), [20.0, 40.0, 100.0, 80.0]);
        assert_eq!((rect[2] - rect[0], rect[3] - rect[1]), (40.0, 80.0));
        assert_eq!((m[0], m[1], m[2], m[3]), (0.0, 1.0, -1.0, 0.0));
    }
}
