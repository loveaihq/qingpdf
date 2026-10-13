//! A page for a word processor (5b): the lines of its text with the box, size and style of every character, where the images
//! are, and the thin lines (rules) the page draws, which are what a ruled table is made of. All in points on the page as it is
//! shown (its own `/Rotate` applied, origin at the top left, y growing downwards), like [`super::PageChars`].

use crate::document::Page;
use crate::error::{Error, Result};
use crate::object::ObjRef;
use crate::render::work::{Work, cost};

use super::chars::{MAX_PAGE_CHARS, Shown};
use super::interp::{Glyph, Matrix, numbers, spent};
use super::layout;
use super::scan::Operand;

/// Most images, rules and path segments one page is read for.
const MAX_IMAGES: usize = 4_000;
const MAX_RULES: usize = 60_000;
const MAX_SEGMENTS: usize = 30_000;
/// A filled rectangle is a rule when it is thinner than this (points) and longer than `MIN_RULE`.
const MAX_RULE_WIDTH: f64 = 2.5;
const MIN_RULE: f64 = 4.0;

/// The text of one character (or of a ligature: several) and where it is on the line's text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    /// Bytes `start..end` of [`TextLine::text`].
    pub start: u32,
    pub end: u32,
    /// `[left, top, right, bottom]` of the glyph's cell.
    pub bbox: [f32; 4],
    pub size: f32,
    /// [`super::font::Font::style`]
    pub style: u8,
}

/// One line of the page's text, as [`super::TextExtractor::page_text`] gives it (without the line feed).
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    /// One piece for each glyph that has text, in the order of the text; the spaces the layout put between glyphs have none.
    pub pieces: Vec<Piece>,
    pub vertical: bool,
}

/// An image the page shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePlace {
    pub obj: ObjRef,
    /// `[left, top, right, bottom]` of the unit square the image is drawn in.
    pub bbox: [f32; 4],
    /// The image is drawn mirrored (left to right, top to bottom).
    pub flip_h: bool,
    pub flip_v: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageLayout {
    /// The size of the page as it is shown.
    pub size: (f32, f32),
    pub lines: Vec<TextLine>,
    pub images: Vec<ImagePlace>,
    /// Thin horizontal and vertical lines `[left, top, right, bottom]` (one of the two sizes is 0).
    pub rules: Vec<[f32; 4]>,
}

enum Seg {
    Line([f64; 4]),
    Rect([f64; 4]),
}

/// What the interpreter notes while it runs the page's content: the images drawn, and the thin lines the paths make.
#[derive(Default)]
pub(super) struct Gfx {
    images: Vec<(ObjRef, Matrix)>,
    /// Thin lines in default user space `[x0, y0, x1, y1]`, `x0 <= x1` and `y0 <= y1`.
    rules: Vec<[f64; 4]>,
    path: Vec<Seg>,
    cur: (f64, f64),
    start: (f64, f64),
}

impl Gfx {
    pub(super) fn image(&mut self, r: ObjRef, ctm: Matrix) {
        if self.images.len() < MAX_IMAGES {
            self.images.push((r, ctm));
        }
    }

    /// A path operator (8.5) or a painting one.
    pub(super) fn operator(&mut self, op: &[u8], ops: &[Operand], ctm: &Matrix) {
        match op {
            b"m" => {
                if let Some([x, y]) = numbers::<2>(ops) {
                    (self.cur, self.start) = ((x, y), (x, y));
                }
            }
            b"l" => {
                if let Some([x, y]) = numbers::<2>(ops) {
                    self.line_to(x, y);
                }
            }
            b"c" => {
                if let Some([.., x, y]) = numbers::<6>(ops) {
                    self.cur = (x, y);
                }
            }
            b"v" | b"y" => {
                if let Some([.., x, y]) = numbers::<4>(ops) {
                    self.cur = (x, y);
                }
            }
            b"re" => {
                if let Some(r) = numbers::<4>(ops)
                    && self.path.len() < MAX_SEGMENTS
                {
                    self.path.push(Seg::Rect(r));
                    (self.cur, self.start) = ((r[0], r[1]), (r[0], r[1]));
                }
            }
            b"h" => self.line_to(self.start.0, self.start.1),
            b"S" => self.paint(ctm, true, false),
            b"s" => {
                self.line_to(self.start.0, self.start.1);
                self.paint(ctm, true, false);
            }
            b"f" | b"F" | b"f*" => self.paint(ctm, false, true),
            b"B" | b"B*" => self.paint(ctm, true, true),
            b"b" | b"b*" => {
                self.line_to(self.start.0, self.start.1);
                self.paint(ctm, true, true);
            }
            b"n" => self.path.clear(),
            _ => {}
        }
    }

    fn line_to(&mut self, x: f64, y: f64) {
        if self.path.len() < MAX_SEGMENTS {
            self.path.push(Seg::Line([self.cur.0, self.cur.1, x, y]));
        }
        self.cur = (x, y);
    }

    fn paint(&mut self, ctm: &Matrix, stroke: bool, fill: bool) {
        let path = std::mem::take(&mut self.path);
        let map = |x: f64, y: f64| (ctm[0] * x + ctm[2] * y + ctm[4], ctm[1] * x + ctm[3] * y + ctm[5]);
        for seg in &path {
            match *seg {
                Seg::Line([x0, y0, x1, y1]) => {
                    if stroke {
                        self.rule(map(x0, y0), map(x1, y1));
                    }
                }
                Seg::Rect([x, y, w, h]) => {
                    let p = [map(x, y), map(x + w, y), map(x + w, y + h), map(x, y + h)];
                    if stroke {
                        for k in 0..4 {
                            if let (Some(a), Some(b)) = (p.get(k), p.get((k + 1) % 4)) {
                                self.rule(*a, *b);
                            }
                        }
                    }
                    if fill {
                        let xs = p.iter().map(|q| q.0);
                        let ys = p.iter().map(|q| q.1);
                        let (x0, x1) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
                        let (y0, y1) = (ys.clone().fold(f64::INFINITY, f64::min), ys.fold(f64::NEG_INFINITY, f64::max));
                        let (rw, rh) = (x1 - x0, y1 - y0);
                        if rw.min(rh) <= MAX_RULE_WIDTH && rw.max(rh) >= MIN_RULE {
                            if rw >= rh {
                                self.rule((x0, 0.5 * (y0 + y1)), (x1, 0.5 * (y0 + y1)));
                            } else {
                                self.rule((0.5 * (x0 + x1), y0), (0.5 * (x0 + x1), y1));
                            }
                        }
                    }
                }
            }
        }
        self.path = path;
        self.path.clear();
    }

    fn rule(&mut self, a: (f64, f64), b: (f64, f64)) {
        if !(a.0.is_finite() && a.1.is_finite() && b.0.is_finite() && b.1.is_finite()) || self.rules.len() >= MAX_RULES {
            return;
        }
        if (a.0 - b.0).abs() < 0.5 {
            let x = 0.5 * (a.0 + b.0);
            self.rules.push([x, a.1.min(b.1), x, a.1.max(b.1)]);
        } else if (a.1 - b.1).abs() < 0.5 {
            let y = 0.5 * (a.1 + b.1);
            self.rules.push([a.0.min(b.0), y, a.0.max(b.0), y]);
        }
    }
}

/// The page's layout from what the interpreter found. Charged to `work` by the number of characters, lines and rules.
pub(super) fn build(page: &Page, glyphs: Vec<Glyph>, gfx: Option<Box<Gfx>>, work: &Work) -> Result<PageLayout> {
    if glyphs.len() > MAX_PAGE_CHARS {
        return Err(Error::Limit(format!("more than {MAX_PAGE_CHARS} characters on one page")));
    }
    let gfx = gfx.map(|g| *g).unwrap_or_default();
    if !work.charge(glyphs.len() as f64 * 2.0 * cost::TEXT_LAYOUT + (gfx.rules.len() + gfx.images.len()) as f64 * cost::TEXT_LAYOUT) {
        return Err(spent(work));
    }
    let shown = Shown::of(page);
    let (w, h) = shown.size();
    let mut lines = Vec::new();
    for (text, marks) in layout::lines_marked(&glyphs) {
        let mut pieces = Vec::with_capacity(marks.len());
        let mut vertical = false;
        for m in &marks {
            let Some(g) = glyphs.get(m.glyph) else { continue };
            vertical = g.vertical;
            pieces.push(Piece {
                start: u32::try_from(m.start).unwrap_or(u32::MAX),
                end: u32::try_from(m.end).unwrap_or(u32::MAX),
                bbox: shown.glyph_box(g, 0.0, 1.0),
                size: g.size as f32,
                style: g.style,
            });
        }
        lines.push(TextLine { text, pieces, vertical });
    }
    let on_page = |b: &[f32; 4]| b[2] >= -20.0 && b[0] <= w as f32 + 20.0 && b[3] >= -20.0 && b[1] <= h as f32 + 20.0;
    let mut images = Vec::new();
    for (obj, m) in &gfx.images {
        let at = |x: f64, y: f64| shown.map(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        let (o, u, v) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0));
        let (x0, x1) = (o.0.min(u.0).min(v.0).min(u.0 + v.0 - o.0), o.0.max(u.0).max(v.0).max(u.0 + v.0 - o.0));
        let (y0, y1) = (o.1.min(u.1).min(v.1).min(u.1 + v.1 - o.1), o.1.max(u.1).max(v.1).max(u.1 + v.1 - o.1));
        let bbox = [x0 as f32, y0 as f32, x1 as f32, y1 as f32];
        if bbox.iter().all(|c| c.is_finite()) && on_page(&bbox) && (x1 - x0) >= 2.0 && (y1 - y0) >= 2.0 {
            // The image's x axis runs to the right and its y axis up the page when it is not mirrored.
            images.push(ImagePlace { obj: *obj, bbox, flip_h: u.0 < o.0, flip_v: v.1 > o.1 });
        }
    }
    let mut rules = Vec::new();
    for r in &gfx.rules {
        let (a, b) = (shown.map(r[0], r[1]), shown.map(r[2], r[3]));
        let rule = [a.0.min(b.0) as f32, a.1.min(b.1) as f32, a.0.max(b.0) as f32, a.1.max(b.1) as f32];
        if rule.iter().all(|c| c.is_finite()) && on_page(&rule) {
            rules.push(rule);
        }
    }
    Ok(PageLayout { size: (w as f32, h as f32), lines, images, rules })
}

#[cfg(test)]
mod tests {
    use crate::Document;
    use crate::render::work::Work;
    use crate::testutil::PdfBuilder;
    use crate::text::TextExtractor;
    use crate::text::font::{STYLE_BOLD, STYLE_CJK_SHIFT, STYLE_FAMILY_SHIFT, STYLE_HEAVY, STYLE_ITALIC};

    /// A one-page file (200 by 300, so that y = 300 - y is the page as shown) with `content`, fonts F1 Helvetica, F2 Helvetica-Bold,
    /// F3 SimHei, F4 Courier-Oblique, and an image (2 by 2, object 9) as Im1.
    fn layout_of(content: &str, rotate: i32) -> super::PageLayout {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Rotate {rotate} /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R /F4 8 0 R >> /XObject << /Im1 9 0 R >> >> /Contents 4 0 R >>"));
        b.stream_obj(4, "", content.as_bytes());
        b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
        b.obj(7, "<< /Type /Font /Subtype /TrueType /BaseFont /SimHei /Encoding /WinAnsiEncoding >>");
        b.obj(8, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Oblique >>");
        b.stream_obj(9, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 85, 170, 255]);
        let doc = Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        TextExtractor::new(&doc).page_layout(&pages[0], &Work::new()).unwrap()
    }

    #[test]
    fn the_lines_images_and_thin_lines_of_a_page_are_in_the_page_as_it_is_shown() {
        let l = layout_of("BT /F1 12 Tf 20 200 Td (Hello) Tj ET q 100 0 0 50 40 100 cm /Im1 Do Q 10 10 m 110 10 l S 10 20 100 0.8 re f 0.5 w 150 20 m 150 80 l S 30 250 40 10 re S", 0);
        assert_eq!(l.size, (200.0, 300.0));
        assert_eq!(l.lines.len(), 1);
        assert_eq!(l.lines[0].text, "Hello");
        assert_eq!(l.lines[0].pieces.len(), 5);
        // The image: x 40 to 140, y 100 to 150 on the page, which is 150 to 200 from the top.
        assert_eq!(l.images.len(), 1);
        let b = l.images[0].bbox;
        assert!((b[0] - 40.0).abs() < 0.01 && (b[2] - 140.0).abs() < 0.01 && (b[1] - 150.0).abs() < 0.01 && (b[3] - 200.0).abs() < 0.01, "{b:?}");
        assert!(!l.images[0].flip_h && !l.images[0].flip_v);
        // A stroked line, a thin filled rectangle, a vertical line and the four sides of a stroked rectangle.
        let near = |r: &[f32; 4], want: [f32; 4]| r.iter().zip(want).all(|(a, w)| (a - w).abs() < 0.5);
        assert!(l.rules.iter().any(|r| near(r, [10.0, 290.0, 110.0, 290.0])), "{:?}", l.rules);
        assert!(l.rules.iter().any(|r| near(r, [10.0, 279.6, 110.0, 279.6])), "{:?}", l.rules);
        assert!(l.rules.iter().any(|r| near(r, [150.0, 220.0, 150.0, 280.0])), "{:?}", l.rules);
        assert_eq!(l.rules.len(), 7, "{:?}", l.rules);
    }

    #[test]
    fn a_turned_page_is_given_as_it_is_shown() {
        // The image's lower left corner (40, 100) is, with the page turned a quarter (clockwise), at the top left at (100, 40)... the
        // page is 300 wide and 200 high as shown; the box of the image is 50 wide and 100 high.
        let l = layout_of("q 100 0 0 50 40 100 cm /Im1 Do Q", 90);
        assert_eq!(l.size, (300.0, 200.0));
        let b = l.images[0].bbox;
        assert!(((b[2] - b[0]) - 50.0).abs() < 0.01 && ((b[3] - b[1]) - 100.0).abs() < 0.01, "{b:?}");
    }

    #[test]
    fn a_mirrored_image_says_so() {
        let l = layout_of("q -100 0 0 -50 140 150 cm /Im1 Do Q", 0);
        assert!(l.images[0].flip_h && l.images[0].flip_v, "{:?}", l.images[0]);
        let b = l.images[0].bbox;
        assert!((b[0] - 40.0).abs() < 0.01 && (b[2] - 140.0).abs() < 0.01, "{b:?}");
    }

    #[test]
    fn the_style_of_a_font_is_told_from_its_name() {
        let l = layout_of("BT /F1 12 Tf 10 280 Td (a) Tj ET BT /F2 12 Tf 10 250 Td (b) Tj ET BT /F3 12 Tf 10 220 Td (c) Tj ET BT /F4 12 Tf 10 190 Td (d) Tj ET", 0);
        let style = |text: &str| l.lines.iter().find(|x| x.text == text).unwrap().pieces[0].style;
        let (plain, bold, hei, mono) = (style("a"), style("b"), style("c"), style("d"));
        assert_eq!(plain, 1 << STYLE_FAMILY_SHIFT, "Helvetica is sans, regular");
        assert_eq!(bold, STYLE_BOLD | STYLE_HEAVY | (1 << STYLE_FAMILY_SHIFT));
        assert!(hei & STYLE_HEAVY != 0 && hei & STYLE_BOLD == 0 && (hei >> STYLE_CJK_SHIFT) & 3 == 1, "SimHei is a heavy Hei face: {hei:#x}");
        assert!(mono & STYLE_ITALIC != 0 && (mono >> STYLE_FAMILY_SHIFT) & 3 == 2, "Courier-Oblique is mono, italic: {mono:#x}");
    }

    #[test]
    fn too_many_characters_or_work_are_refused() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>");
        b.stream_obj(4, "", b"BT /F1 12 Tf 10 280 Td (Hello world) Tj ET");
        b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let doc = Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        // A meter that has nothing left: the page is refused, with a limit and not a panic.
        let spent = Work::with_allowance(10.0);
        assert!(matches!(TextExtractor::new(&doc).page_layout(&pages[0], &spent), Err(crate::Error::Limit(_))));
    }
}
