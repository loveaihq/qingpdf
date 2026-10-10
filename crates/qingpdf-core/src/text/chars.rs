//! Where the characters of a page are (3d-2): for a reader's selection and search highlights, every character of the page's
//! text gets the box it covers on the page as it is shown.

use crate::document::Page;

use super::interp::Glyph;
use super::visible_box;

/// Most characters of one page the reader works with (the rest of a page with more is left out).
pub const MAX_PAGE_CHARS: usize = 400_000;

/// The text of a page, the way [`super::TextExtractor::page_text`] gives it, with the box of every character.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageChars {
    pub text: String,
    /// One box for each `char` of `text`, in the same order: `[left, top, right, bottom]` in points on the page as it is
    /// shown (its own `/Rotate` applied, the origin at the top left, y growing downwards). A space the layout put between
    /// two pieces and the end of a line have no box of their own: a point (left = right, top = bottom).
    pub boxes: Vec<[f32; 4]>,
}

/// User space to the page as it is shown (the crop box, turned by `/Rotate`, 8.3.4 and 14.11.2), the same transform the
/// renderer uses.
pub(crate) struct Shown {
    rotate: i64,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Shown {
    pub(crate) fn of(page: &Page) -> Shown {
        let [x0, y0, x1, y1] = visible_box(page).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        Shown { rotate: page.rotate().rem_euclid(360), x0, y0, x1, y1 }
    }

    /// A point of user space on the page as it is shown, in points from the top left.
    pub(crate) fn map(&self, x: f64, y: f64) -> (f64, f64) {
        match self.rotate {
            90 => (y - self.y0, x - self.x0),
            180 => (self.x1 - x, y - self.y0),
            270 => (self.y1 - y, self.x1 - x),
            _ => (x - self.x0, self.y1 - y),
        }
    }

    /// Where a destination's `left` (the left edge of the crop box when there is none) and `top` are on the page as shown.
    pub(crate) fn point(&self, left: Option<f64>, top: f64) -> (f64, f64) {
        self.map(left.unwrap_or(self.x0), top)
    }

    /// The box of the part of the glyph's cell from `from` to `to` (fractions of its advance, for a glyph that stands for
    /// several characters, like a ligature).
    pub(super) fn glyph_box(&self, g: &Glyph, from: f64, to: f64) -> [f32; 4] {
        // The cell runs along the writing direction from the origin; across it, a horizontal glyph sits on its baseline
        // (a little below it for the descenders, most of the size above), a vertical one is centred on its column.
        let (across0, across1) = if g.vertical { (-0.5 * g.size, 0.5 * g.size) } else { (-0.2 * g.size, 0.8 * g.size) };
        let (nx, ny) = (-g.dy, g.dx);
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for along in [g.adv * from, g.adv * to] {
            for across in [across0, across1] {
                let (x, y) = self.map(g.x + along * g.dx + across * nx, g.y + along * g.dy + across * ny);
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        let f = |v: f64| if v.is_finite() { v.clamp(-1.0e6, 1.0e6) as f32 } else { 0.0 };
        [f(b[0]), f(b[1]), f(b[2]), f(b[3])]
    }
}
