//! Cropping pages (2026-10-21): the part of the page that is shown is the crop box (14.11.2), so cropping is writing `/CropBox` on the page and
//! uncropping is giving it the crop box it had. Nothing on the page is touched: the content, the annotations and the links keep their
//! places, only less of the page is shown. The crop box is clipped to the media box and to the box that is shown now.
//!
//! A page that had a crop box of its own when it was first cropped here keeps it, in `/QingpdfCropBefore` of the page (an array of four numbers; an
//! empty one when the page had none of its own: it showed its media box or the crop box of the page tree), and uncropping puts it back (or takes the
//! page's own crop box away): a file made to show less than its media box (crop marks outside the crop box, say) does not show more after the
//! uncropping than before the cropping. A page cropped by something else, and so with no such note, gets the media box.
//!
//! The places are those of the page as it is shown (points from its top left corner, the page's own `/Rotate` applied, like the boxes of the
//! characters), so margins and a dragged rectangle mean what the person sees on a turned page too; [`Shown`] turns them into user space.
//! The plain parts (the margins, the box of what a page draws) are here with their tests.

use super::content::Added;
use super::outline::added_by;
use super::{Ctx, LIMIT};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::layout::reals;
use crate::object::Object;
use crate::render::Renderer;
use crate::text::{Shown, visible_box};

/// Points in a millimetre.
pub const PT_PER_MM: f64 = 72.0 / 25.4;
/// The least a crop leaves of a page, in points, both ways.
pub const MIN_SIDE: f64 = 1.0;
/// The most a margin may be, in points (the pages are at most 14,400 points).
pub const MAX_MARGIN: f64 = 14_400.0;
/// What is left round the content of a page by "crop the white margins", in millimetres.
pub const AUTO_PAD_MM: f64 = 3.0;
/// A pixel whose colours are all within this many levels of the colour of the paper is paper (a scan has paper of a grey or a cream and is not
/// the same colour all over).
pub const PAPER_TOLERANCE: u8 = 20;
/// A rectangle that is this much of the width and of the height of a page or more leaves no margin to speak of.
pub const NEARLY_WHOLE: f64 = 0.98;
/// The resolution a page is drawn at to find where its content is, and the most pixels of that picture (a bigger page is drawn smaller).
pub const AUTO_DPI: f64 = 36.0;
const AUTO_PIXELS: f64 = 1.5e6;
/// What the work of finding the content box of one page may come to is the drawing's own; this is what it is charged to the pages' count.
const EDIT_WORK: f64 = 2.0e6;

/// The key of the page where the crop box it had before it was cropped here is kept.
const BEFORE: &str = "QingpdfCropBefore";

/// A change to the crop boxes of pages. Pages are counted from 0.
#[derive(Clone, Debug, PartialEq)]
pub enum CropOp {
    /// Each page listed gets this rectangle, `[left, top, right, bottom]` of the page as it is shown now, as its crop box.
    Set(Vec<(u32, [f32; 4])>),
    /// The pages listed show their whole media box again.
    Reset(Vec<u32>),
}

fn odd(what: &str) -> Error {
    Error::Invalid(what.to_string())
}

/// The rectangle of a page of `size` points (as shown) that is left when `[top, right, bottom, left]` points are cut off its sides.
pub fn margins_rect((w, h): (f64, f64), margins: [f64; 4]) -> Result<[f64; 4]> {
    if margins.iter().any(|m| !m.is_finite() || *m < 0.0 || *m > MAX_MARGIN) {
        return Err(odd("a margin is a number of millimetres, not less than 0"));
    }
    let [top, right, bottom, left] = margins;
    let rect = [left, top, w - right, h - bottom];
    if rect[2] - rect[0] < MIN_SIDE || rect[3] - rect[1] < MIN_SIDE {
        return Err(odd("the margins take up the whole page"));
    }
    Ok(rect)
}

/// The colour of the paper of a picture (`rgba`): the colour that most pixels have, counted in steps of 16 levels a channel, as the mean of the pixels
/// in the commonest step. (A page that is white with some text is white; a scan on grey paper is grey; a page that is one big picture has a colour
/// that stands for nothing, and has no margin either way.)
pub fn paper_colour(rgba: &[u8]) -> [u8; 3] {
    // (For each step: how many pixels, and the sum of their colours.)
    let mut steps: Vec<(u32, [u64; 3])> = vec![(0, [0; 3]); 4096];
    for p in rgba.as_chunks::<4>().0 {
        let bin = (usize::from(p[0] >> 4) << 8) | (usize::from(p[1] >> 4) << 4) | usize::from(p[2] >> 4);
        if let Some(step) = steps.get_mut(bin) {
            step.0 += 1;
            for (s, &c) in step.1.iter_mut().zip(p) {
                *s += u64::from(c);
            }
        }
    }
    // (The first of the commonest steps if there are several.)
    let (count, sum) = steps.iter().fold((0u32, [0u64; 3]), |best, step| if step.0 > best.0 { *step } else { best });
    let n = u64::from(count.max(1));
    sum.map(|s| u8::try_from(s / n).unwrap_or(255))
}

/// The first and last row and column (`[left, top, right, bottom]`, the last ones not included) of a picture of `w` by `h` pixels (`rgba`) that have a pixel
/// that is not paper (more than [`PAPER_TOLERANCE`] levels from the paper's colour in one channel); `None` for a blank page.
pub fn content_pixels(rgba: &[u8], w: usize, h: usize) -> Option<[usize; 4]> {
    let mut found: Option<[usize; 4]> = None;
    let paper = paper_colour(rgba.get(..(w * h * 4).min(rgba.len())).unwrap_or(rgba));
    for (y, row) in rgba.chunks_exact((w * 4).max(4)).take(h).enumerate() {
        let pixels = row.as_chunks::<4>().0;
        let ink = |p: &&[u8; 4]| p.iter().zip(paper).any(|(&c, paper)| c.abs_diff(paper) > PAPER_TOLERANCE);
        let (Some(first), Some(last)) = (pixels.iter().position(|p| ink(&p)), pixels.iter().rposition(|p| ink(&p))) else { continue };
        found = Some(match found {
            None => [first, y, last + 1, y + 1],
            Some(b) => [b[0].min(first), b[1], b[2].max(last + 1), y + 1],
        });
    }
    found
}

/// The rectangle (`[left, top, right, bottom]` points of a page as shown, `size` points) of the pixels `px` of a picture of the page that is `bitmap`
/// pixels, with `pad` points round it, kept on the page.
pub fn auto_rect(px: [usize; 4], bitmap: (usize, usize), size: (f64, f64), pad: f64) -> [f64; 4] {
    let (sx, sy) = (size.0 / bitmap.0.max(1) as f64, size.1 / bitmap.1.max(1) as f64);
    let grown = |v: f64, s: f64, low: bool, most: f64| if low { (v * s - pad).max(0.0) } else { (v * s + pad).min(most) };
    [grown(px[0] as f64, sx, true, size.0), grown(px[1] as f64, sy, true, size.1), grown(px[2] as f64, sx, false, size.0), grown(px[3] as f64, sy, false, size.1)]
}

/// Does cropping a page of `size` points (as shown) to `rect` leave out so little that there is no margin to cut?
pub fn is_nearly_whole(rect: [f64; 4], (w, h): (f64, f64)) -> bool {
    rect[2] - rect[0] >= w * NEARLY_WHOLE && rect[3] - rect[1] >= h * NEARLY_WHOLE
}

/// The resolution to draw a page of `size` points at to look for its content: [`AUTO_DPI`], less for a page so big that it would be more than
/// [`AUTO_PIXELS`] pixels.
pub fn auto_dpi((w, h): (f64, f64)) -> f64 {
    let fits = (AUTO_PIXELS / (w * h).max(1.0)).sqrt() * 72.0;
    AUTO_DPI.min(fits).max(crate::render::MIN_DPI)
}

/// Where the content of `page` is: the rectangle of the page as shown that holds all that is drawn on it, a little more all round (`pad` points);
/// `None` for a page that draws nothing. A page that cannot be drawn is an error (or stopped, with `Cancelled`).
#[inline(never)]
pub fn auto_box(renderer: &mut Renderer<'_>, page: &Page, pad: f64) -> Result<Option<[f64; 4]>> {
    let size = Shown::of(page).size();
    let bitmap = renderer.render_page(page, auto_dpi(size))?;
    let (w, h) = (bitmap.width as usize, bitmap.height as usize);
    Ok(content_pixels(&bitmap.rgba, w, h).map(|px| auto_rect(px, (w, h), size, pad)))
}

/// A number written with two decimals at most.
fn cents(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The crop box of `page` that shows the rectangle `rect` of the page as it is shown now, in user space: inside the box that is shown now.
#[inline(never)]
fn crop_box_for(page: &Page, rect: [f32; 4]) -> Result<[f64; 4]> {
    let visible = visible_box(page).ok_or_else(|| odd("the page has no usable media box"))?;
    if rect.iter().any(|v| !v.is_finite() || f64::from(v.abs()) > LIMIT) {
        return Err(odd("the rectangle is not on the page"));
    }
    let shown = Shown::of(page);
    let (a, b) = (shown.unmap(f64::from(rect[0]), f64::from(rect[1])), shown.unmap(f64::from(rect[2]), f64::from(rect[3])));
    let got = [a.0.min(b.0).max(visible[0]), a.1.min(b.1).max(visible[1]), a.0.max(b.0).min(visible[2]), a.1.max(b.1).min(visible[3])];
    if got[2] - got[0] < MIN_SIDE || got[3] - got[1] < MIN_SIDE {
        return Err(odd("the rectangle is outside the page or leaves nothing of it"));
    }
    Ok(got.map(cents))
}

/// Work out `op`: each page it names, written again with its crop box.
#[inline(never)]
pub(super) fn apply(ctx: &mut Ctx<'_>, pages: &[Page], op: &CropOp) -> Result<()> {
    if !ctx.work.charge(EDIT_WORK + 1.0e4 * pages.len() as f64) {
        return Err(Error::Limit("too many changes in this session: save the file and open it again".to_string()));
    }
    let from = ctx.objects.len();
    // The pages to change, each with the crop box it is to show (`None`: the one it had).
    let mut plan: Vec<(&Page, Option<[f64; 4]>)> = Vec::new();
    let page_of = |index: u32| pages.get(index as usize).ok_or_else(|| odd("there is no such page"));
    match op {
        CropOp::Set(list) => {
            for &(index, rect) in list {
                let page = page_of(index)?;
                plan.push((page, Some(crop_box_for(page, rect)?)));
            }
        }
        CropOp::Reset(list) => {
            for &index in list {
                plan.push((page_of(index)?, None));
            }
        }
    }
    let mut done: Vec<u32> = Vec::new();
    for (page, new) in plan {
        if done.contains(&page.obj_ref.num) {
            continue;
        }
        done.push(page.obj_ref.num);
        let Ok(Object::Dict(mut dict)) = ctx.doc.get(page.obj_ref) else { return Err(odd("a page is not a dictionary")) };
        let before = dict.get(BEFORE).and_then(|b| ctx.doc.resolve(b).ok());
        match new {
            Some(new) => {
                // The first crop made here remembers the crop box the page had of its own.
                if before.is_none() {
                    let own = match dict.get("CropBox").and_then(|c| ctx.doc.resolve(c).ok()) {
                        Some(Object::Array(a)) if a.len() == 4 => {
                            let numbers: Vec<f64> = a.iter().filter_map(|v| ctx.doc.resolve(v).ok().and_then(|o| o.as_f64())).collect();
                            if numbers.len() == 4 { reals(&numbers) } else { Object::Array(Vec::new()) }
                        }
                        _ => Object::Array(Vec::new()),
                    };
                    dict.set(BEFORE, own);
                }
                dict.set("CropBox", reals(&new));
            }
            None => match before {
                // The crop box the page had, or none of its own.
                Some(Object::Array(a)) => {
                    if a.len() == 4 {
                        dict.set("CropBox", Object::Array(a));
                    } else {
                        dict.remove("CropBox");
                    }
                    dict.remove(BEFORE);
                }
                // A page that shows all its media box has nothing to be given back; one that was cropped by something else shows it again.
                _ => match page.media_box().filter(|m| Some(*m) != visible_box(page)) {
                    Some(media) => dict.set("CropBox", reals(&media)),
                    None => continue,
                },
            },
        }
        ctx.objects.push((page.obj_ref.num, Object::Dict(dict)));
    }
    if ctx.objects.len() == from {
        return Err(odd(if matches!(op, CropOp::Reset(_)) { "none of the pages is cropped" } else { "there is nothing to crop" }));
    }
    ctx.tally(from)
}

/// What the command line crops by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CropHow {
    /// `[top, right, bottom, left]` in points, cut off every page.
    Margins([f64; 4]),
    /// The white round the content is cut off, leaving [`AUTO_PAD_MM`].
    Auto,
}

/// What [`crop_file`] did.
#[derive(Debug)]
pub struct Cropped {
    pub added: Added,
    /// The pages cropped, the pages with nothing on them and the pages whose content fills them to the edges (those two left as they are).
    pub cropped: Vec<usize>,
    pub blank: Vec<usize>,
    pub whole: Vec<usize>,
}

/// The file with the pages `which` (all of them, `None`) cropped by `how`, as one incremental update behind the whole original (a file with damaged
/// structure data is written afresh first). The permissions (bit 11 or bit 4, unless it was opened with the owner password) and the signatures are
/// looked at.
pub fn crop_file(doc: &Document, password: &str, how: CropHow, which: Option<&[usize]>) -> Result<Cropped> {
    super::outline::check_allowed(doc, password, "cropping its pages")?;
    let mut summary: (Vec<usize>, Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new(), Vec::new());
    let added = added_by(doc, password, |ctx, pages| {
        let chosen: Vec<usize> = match which {
            Some(list) => list.iter().copied().filter(|&p| p < pages.len()).collect(),
            None => (0..pages.len()).collect(),
        };
        let mut set = Vec::with_capacity(chosen.len());
        let mut renderer = Renderer::new(ctx.doc, crate::forms::painter());
        for &index in &chosen {
            let Some(page) = pages.get(index) else { continue };
            let size = Shown::of(page).size();
            let rect = match how {
                CropHow::Margins(m) => Some(margins_rect(size, m)?),
                CropHow::Auto => {
                    if !ctx.work.charge(EDIT_WORK) {
                        return Err(Error::Limit("too many pages to look at".to_string()));
                    }
                    auto_box(&mut renderer, page, AUTO_PAD_MM * PT_PER_MM)?
                }
            };
            match rect {
                Some(r) if matches!(how, CropHow::Auto) && is_nearly_whole(r, size) => summary.2.push(index),
                Some(r) => {
                    set.push((index as u32, r.map(|v| v as f32)));
                    summary.0.push(index);
                }
                None => summary.1.push(index),
            }
        }
        if set.is_empty() && matches!(how, CropHow::Auto) {
            return Err(odd("there is no white margin to cut on those pages (they are blank, or what they draw fills them)"));
        }
        apply(ctx, pages, &CropOp::Set(set))
    })?;
    Ok(Cropped { added, cropped: summary.0, blank: summary.1, whole: summary.2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Edit, apply_any, commit, save_section};
    use crate::render::work::Work;
    use crate::testutil::PdfBuilder;

    fn apply_edit(doc: &Document, pages: &[Page], edit: &Edit, now: &str) -> Result<Vec<(u32, Object)>> {
        apply_any(doc, pages, edit, now, &Work::new(), 16 << 20).map(|(objects, _)| objects)
    }

    fn doc_with(page_extra: &str) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /Resources << >> {page_extra} >>"));
        b.finish_classic(20, "/Root 1 0 R")
    }

    fn crop_boxes(base: &[u8], op: CropOp) -> Vec<f64> {
        let doc = Document::from_bytes(base.to_vec()).unwrap();
        let pages = doc.pages().unwrap();
        let objects = apply_edit(&doc, &pages, &Edit::Crop(Box::new(op)), "").unwrap();
        commit(&doc, objects);
        let mut bytes = base.to_vec();
        bytes.extend_from_slice(&save_section(&doc).unwrap());
        let again = Document::from_bytes(bytes).unwrap();
        let page = &again.pages().unwrap()[0];
        page.crop_box().unwrap().to_vec()
    }

    #[test]
    fn margins_are_cut_off_the_page_as_it_is_shown() {
        assert_eq!(margins_rect((200.0, 100.0), [10.0, 20.0, 30.0, 40.0]).unwrap(), [40.0, 10.0, 180.0, 70.0]);
        assert_eq!(margins_rect((200.0, 100.0), [0.0; 4]).unwrap(), [0.0, 0.0, 200.0, 100.0]);
        for bad in [[-1.0, 0.0, 0.0, 0.0], [f64::NAN, 0.0, 0.0, 0.0], [0.0, f64::INFINITY, 0.0, 0.0], [60.0, 0.0, 40.0, 0.0], [0.0, 100.0, 0.0, 100.0], [20_000.0, 0.0, 0.0, 0.0]] {
            assert!(margins_rect((200.0, 100.0), bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_box_of_the_content_is_its_pixels_with_a_pad_kept_on_the_page() {
        let mut rgba = vec![255u8; 20 * 10 * 4];
        assert_eq!(content_pixels(&rgba, 20, 10), None, "paper");
        // Within the tolerance of the paper is paper too; a step further is not.
        let mut put = |x: usize, y: usize, v: u8| rgba[(y * 20 + x) * 4..(y * 20 + x) * 4 + 3].fill(v);
        put(2, 1, 255 - PAPER_TOLERANCE);
        put(7, 5, 100);
        put(15, 8, 0);
        assert_eq!(content_pixels(&rgba, 20, 10), Some([7, 5, 16, 9]));
        let mut one = rgba.clone();
        one.iter_mut().for_each(|v| *v = 255);
        one[(3 * 20 + 4) * 4 + 1] = 255 - PAPER_TOLERANCE - 1;
        assert_eq!(content_pixels(&one, 20, 10), Some([4, 3, 5, 4]));
        // 20 by 10 pixels for a page of 200 by 100 points: ten points a pixel; a pad of 8 and the page's own edge.
        assert_eq!(auto_rect([7, 5, 16, 9], (20, 10), (200.0, 100.0), 8.0), [62.0, 42.0, 168.0, 98.0]);
        assert_eq!(auto_rect([0, 0, 20, 10], (20, 10), (200.0, 100.0), 8.0), [0.0, 0.0, 200.0, 100.0]);
        // A short buffer is not read past its end.
        assert_eq!(content_pixels(&rgba[..40], 20, 10), None);
    }

    fn page_drawing(content: &str) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>");
        b.stream_obj(4, "", content.as_bytes());
        b.finish_classic(20, "/Root 1 0 R")
    }

    #[test]
    fn the_paper_is_the_commonest_colour_and_not_always_white() {
        // A scan on grey paper (232 is darker than the old limit of 244): the margins are the paper, the block is the content.
        let grey = Document::from_bytes(page_drawing("0.91 g 0 0 200 100 re f 0 g 60 30 80 40 re f")).unwrap();
        let done = crop_file(&grey, "", CropHow::Auto, None).unwrap();
        assert_eq!((done.cropped.clone(), done.blank.len(), done.whole.len()), (vec![0], 0, 0));
        let page = Document::from_bytes(done.added.data.clone()).unwrap().pages().unwrap().remove(0);
        let got = page.crop_box().unwrap();
        // The block is 60..140 across and 30..70 up; the pad is 3 millimetres (8.5 points); a pixel of the look is 2 points.
        for (g, want) in got.iter().zip([51.5, 21.5, 148.5, 78.5]) {
            assert!((g - want).abs() <= 3.0, "{got:?}");
        }
        // Paper that is not the same all over (a scan's shading) is paper still.
        let mut rgba = vec![0u8; 40 * 20 * 4];
        for (i, p) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let shade = 225 + (i % 40) as u8 / 4;
            *p = [shade, shade - 3, shade - 12, 255];
        }
        assert_eq!(content_pixels(&rgba, 40, 20), None);
        assert!(paper_colour(&rgba)[0] >= 225);
        // A page that is one big picture has no margin to cut: what it draws fills it, and the crop says so instead of doing nothing.
        let picture = Document::from_bytes(page_drawing("1 0 0 rg 0 50 100 50 re f 0 1 0 rg 100 50 100 50 re f 0 0 1 rg 0 0 100 50 re f 1 1 0 rg 100 0 100 50 re f")).unwrap();
        match crop_file(&picture, "", CropHow::Auto, None) {
            Err(Error::Invalid(why)) => assert!(why.contains("no white margin"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(is_nearly_whole([0.0, 0.0, 200.0, 100.0], (200.0, 100.0)) && is_nearly_whole([1.0, 1.0, 199.0, 99.5], (200.0, 100.0)));
        assert!(!is_nearly_whole([10.0, 0.0, 200.0, 100.0], (200.0, 100.0)) && !is_nearly_whole([0.0, 0.0, 200.0, 50.0], (200.0, 100.0)));
        // White paper with text is still cropped, the same as before.
        let white = Document::from_bytes(page_drawing("0 g 60 30 80 40 re f")).unwrap();
        assert_eq!(crop_file(&white, "", CropHow::Auto, None).unwrap().cropped, vec![0]);
    }

    #[test]
    fn the_dpi_of_the_look_is_kept_for_the_biggest_pages() {
        assert_eq!(auto_dpi((595.0, 842.0)), AUTO_DPI);
        let big = auto_dpi((14_400.0, 14_400.0));
        assert!(big < 7.0 && (14_400.0 * big / 72.0).powi(2) <= AUTO_PIXELS * 1.01, "{big}");
        assert_eq!(auto_dpi((1.0e9, 1.0e9)), crate::render::MIN_DPI);
    }

    #[test]
    fn the_crop_box_follows_the_turn_and_the_box_that_is_shown() {
        // Shown 200 wide, 100 high; the top left quarter of what is shown.
        let plain = doc_with("/MediaBox [0 0 200 100]");
        assert_eq!(crop_boxes(&plain, CropOp::Set(vec![(0, [0.0, 0.0, 100.0, 50.0])])), vec![0.0, 50.0, 100.0, 100.0]);
        // Turned a quarter, the page is shown 100 wide and 200 high: the top left corner is the lower left corner of user space turned.
        let turned = doc_with("/MediaBox [0 0 200 100] /Rotate 90");
        assert_eq!(crop_boxes(&turned, CropOp::Set(vec![(0, [0.0, 0.0, 50.0, 100.0])])), vec![0.0, 0.0, 100.0, 50.0]);
        let upside = doc_with("/MediaBox [0 0 200 100] /Rotate 180");
        assert_eq!(crop_boxes(&upside, CropOp::Set(vec![(0, [0.0, 0.0, 50.0, 25.0])])), vec![150.0, 0.0, 200.0, 25.0]);
        let left = doc_with("/MediaBox [0 0 200 100] /Rotate 270");
        assert_eq!(crop_boxes(&left, CropOp::Set(vec![(0, [0.0, 0.0, 50.0, 100.0])])), vec![100.0, 50.0, 200.0, 100.0]);
        // Inside the crop box the page has now: a media box that does not start at 0 and a crop box inside it.
        let boxed = doc_with("/MediaBox [100 100 400 300] /CropBox [150 120 350 280]");
        assert_eq!(crop_boxes(&boxed, CropOp::Set(vec![(0, [10.0, 20.0, 60.0, 60.0])])), vec![160.0, 220.0, 210.0, 260.0]);
        // A rectangle that sticks out is cut to the page; one wholly outside, or with no size, is refused.
        assert_eq!(crop_boxes(&plain, CropOp::Set(vec![(0, [-50.0, -50.0, 80.0, 40.0])])), vec![0.0, 60.0, 80.0, 100.0]);
        let doc = Document::from_bytes(plain.clone()).unwrap();
        let pages = doc.pages().unwrap();
        for bad in [[300.0, 0.0, 400.0, 50.0], [10.0, 10.0, 10.0, 50.0], [f32::NAN, 0.0, 50.0, 50.0], [0.0, 0.0, 5.0e9, 50.0]] {
            assert!(apply_edit(&doc, &pages, &Edit::Crop(Box::new(CropOp::Set(vec![(0, bad)]))), "").is_err(), "{bad:?}");
        }
        assert!(apply_edit(&doc, &pages, &Edit::Crop(Box::new(CropOp::Set(vec![(5, [0.0, 0.0, 50.0, 50.0])]))), "").is_err(), "no such page");
        assert!(apply_edit(&doc, &pages, &Edit::Crop(Box::new(CropOp::Set(Vec::new()))), "").is_err(), "nothing chosen");
    }

    /// The crop box of the first page (the one it has or gets from the page tree) after each of the operations, each made on the file the one before wrote.
    fn crop_boxes_through(base: &[u8], ops: Vec<CropOp>) -> Vec<Option<[f64; 4]>> {
        let mut bytes = base.to_vec();
        let mut seen = Vec::new();
        for op in ops {
            let doc = Document::from_bytes(bytes.clone()).unwrap();
            let pages = doc.pages().unwrap();
            let objects = apply_edit(&doc, &pages, &Edit::Crop(Box::new(op)), "").unwrap();
            commit(&doc, objects);
            bytes.extend_from_slice(&save_section(&doc).unwrap());
            seen.push(Document::from_bytes(bytes.clone()).unwrap().pages().unwrap()[0].crop_box());
        }
        seen
    }

    #[test]
    fn uncropping_puts_back_the_crop_box_the_page_had() {
        let set = |r: [f32; 4]| CropOp::Set(vec![(0, r)]);
        // A page made to show less than its media box (crop marks outside the crop box): after the cropping and the uncropping it shows what it did.
        let own = doc_with("/MediaBox [0 0 300 200] /CropBox [20 20 280 180]");
        let seen = crop_boxes_through(&own, vec![set([50.0, 50.0, 100.0, 100.0]), CropOp::Reset(vec![0])]);
        assert_eq!(seen, vec![Some([70.0, 80.0, 120.0, 130.0]), Some([20.0, 20.0, 280.0, 180.0])]);
        // Cropped twice, uncropped once: the box it had before the first crop (the note is not written over).
        let seen = crop_boxes_through(&own, vec![set([50.0, 50.0, 100.0, 100.0]), set([5.0, 5.0, 20.0, 20.0]), CropOp::Reset(vec![0])]);
        assert_eq!(seen.last(), Some(&Some([20.0, 20.0, 280.0, 180.0])));
        // A page with none: the crop box it was given is taken away again.
        let none = doc_with("/MediaBox [0 0 300 200]");
        let seen = crop_boxes_through(&none, vec![set([50.0, 50.0, 100.0, 100.0]), CropOp::Reset(vec![0])]);
        assert_eq!(seen, vec![Some([50.0, 100.0, 100.0, 150.0]), None]);
        // One that gets its crop box from the page tree shows that again, not the media box.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 200] /CropBox [10 10 250 150] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Resources << >> >>");
        let inherited = b.finish_classic(20, "/Root 1 0 R");
        let seen = crop_boxes_through(&inherited, vec![set([20.0, 20.0, 60.0, 60.0]), CropOp::Reset(vec![0])]);
        assert_eq!(seen[0], Some([30.0, 90.0, 70.0, 130.0]));
        assert_eq!(seen[1], Some([10.0, 10.0, 250.0, 150.0]));
        // A page cropped by something else (no note) shows its media box again.
        assert_eq!(crop_boxes_through(&inherited, vec![CropOp::Reset(vec![0])]), vec![Some([0.0, 0.0, 300.0, 200.0])]);
    }

    #[test]
    fn uncropping_gives_back_the_media_box_and_a_page_that_is_whole_is_left_alone() {
        let cropped = doc_with("/MediaBox [0 0 300 200] /CropBox [10 10 100 100]");
        assert_eq!(crop_boxes(&cropped, CropOp::Reset(vec![0])), vec![0.0, 0.0, 300.0, 200.0]);
        let whole = Document::from_bytes(doc_with("/MediaBox [0 0 300 200]")).unwrap();
        let pages = whole.pages().unwrap();
        assert!(apply_edit(&whole, &pages, &Edit::Crop(Box::new(CropOp::Reset(vec![0]))), "").is_err());
    }
}
