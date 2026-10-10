//! How a page is put on the paper (3d-2): which way it is turned, how big it is drawn and where the bands of it go in the
//! printer's pixels. Plain arithmetic; the printer itself is `win32.rs`'s.

/// The most dots per inch a page is drawn at; a printer of a finer resolution has the picture stretched.
pub const MAX_DPI: f64 = 300.0;
/// Most pages one print job holds.
pub const MAX_PAGES: usize = qingpdf_core::view::MAX_PRINT_PAGES;

/// How one page goes on the paper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// A further turn of the page in degrees, 0 or 90 (a landscape page on portrait paper is turned).
    pub rotation: u16,
    /// The dots per inch the page is drawn at.
    pub dpi: f64,
    /// Printer pixels to a pixel of the drawn page (1 for a printer of no more than [`MAX_DPI`]).
    pub factor: f64,
}

/// How a page of `w` by `h` points goes on paper that can be printed on over `width` by `height` pixels at `printer_dpi`:
/// the right way up for the paper, at its own size if it fits and shrunk to fit if not (never made bigger).
pub fn fit(w: f64, h: f64, width: i32, height: i32, printer_dpi: i32) -> Fit {
    let printer_dpi = f64::from(printer_dpi.max(1));
    let (paper_w, paper_h) = (f64::from(width.max(1)) * 72.0 / printer_dpi, f64::from(height.max(1)) * 72.0 / printer_dpi);
    let (w, h) = (w.max(0.01), h.max(0.01));
    let upright = (paper_w / w).min(paper_h / h).min(1.0);
    let turned = (paper_w / h).min(paper_h / w).min(1.0);
    let (rotation, scale) = if turned > upright * 1.000_001 { (90, turned) } else { (0, upright) };
    let drawn_dpi = printer_dpi.min(MAX_DPI);
    Fit { rotation, dpi: drawn_dpi * scale, factor: printer_dpi / drawn_dpi }
}

/// Printer pixels to a pixel of the page as the engine drew it, at `drawn_dpi`, which may be less than the `dpi` of the [`Fit`]
/// (a file that allows only low quality printing is drawn at 150 dpi at most): the page is as big on the paper whatever it was drawn
/// at. When the engine drew at the fit's own dpi this is [`Fit::factor`].
pub fn pixel_factor(fit: Fit, drawn_dpi: f64) -> f64 {
    if drawn_dpi.is_finite() && drawn_dpi > 0.0 { fit.factor * fit.dpi / drawn_dpi } else { fit.factor }
}

/// Where a page of `page_w` by `page_h` drawn pixels goes on paper of `width` by `height` printer pixels: centred.
pub fn origin(page_w: u32, page_h: u32, factor: f64, width: i32, height: i32) -> (i32, i32) {
    let (pw, ph) = (f64::from(page_w) * factor, f64::from(page_h) * factor);
    (((f64::from(width) - pw) / 2.0).floor().max(0.0) as i32, ((f64::from(height) - ph) / 2.0).floor().max(0.0) as i32)
}

/// The place of a band of rows `y` to `y + h` of a drawn page, `w` pixels across, on the paper: `(x, y, width, height)` in
/// printer pixels, the edges rounded so that bands that follow each other meet without a gap or an overlap.
pub fn band_dest(origin: (i32, i32), factor: f64, y: u32, w: u32, h: u32) -> (i32, i32, i32, i32) {
    let edge = |v: u32| (f64::from(v) * factor).round() as i32;
    let (top, bottom) = (edge(y), edge(y.saturating_add(h)));
    (origin.0, origin.1.saturating_add(top), edge(w).max(1), (bottom - top).max(1))
}

/// The pages (from 0) the ranges name, in order, each page once: `ranges` are first and last page counted from 1; none given
/// is all of the `total` pages. Ranges that run outside the document are cut to it.
pub fn pages_of(ranges: &[(u32, u32)], total: u32) -> Vec<u32> {
    if ranges.is_empty() {
        return (0..total).take(MAX_PAGES).collect();
    }
    let mut seen = vec![false; total as usize];
    let mut out = Vec::new();
    for &(a, b) in ranges {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        for p in a.max(1)..=b.min(total) {
            if let Some(slot) = seen.get_mut(p as usize - 1)
                && !*slot
                && out.len() < MAX_PAGES
            {
                *slot = true;
                out.push(p - 1);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_that_fits_is_printed_at_its_own_size_and_one_that_does_not_is_shrunk() {
        // US Letter paper with no margin at 600 dpi: 5100 by 6600 pixels (8.5 by 11 inches).
        let f = fit(595.0, 842.0, 5100, 6600, 600);
        // A4 is a little too tall: scaled by 792/842.
        assert_eq!(f.rotation, 0);
        assert!((f.dpi - 300.0 * 792.0 / 842.0).abs() < 1e-9, "{f:?}");
        assert_eq!(f.factor, 2.0);
        // A small page is not made bigger.
        let small = fit(300.0, 400.0, 5100, 6600, 600);
        assert_eq!((small.rotation, small.dpi, small.factor), (0, 300.0, 2.0));
        // A printer of 300 dpi or less: the drawn pixels are the printer's.
        let lo = fit(612.0, 792.0, 2550, 3300, 300);
        assert_eq!((lo.dpi, lo.factor), (300.0, 1.0));
        let draft = fit(612.0, 792.0, 1275, 1650, 150);
        assert_eq!((draft.dpi, draft.factor), (150.0, 1.0));
    }

    #[test]
    fn a_page_drawn_at_a_lower_dpi_than_asked_still_fills_the_same_paper() {
        // US Letter at 300 dpi on 2550 by 3300: asked at 300 dpi, but the file allows 150 dpi only, so the engine drew 1275 by 1650.
        let f = fit(612.0, 792.0, 2550, 3300, 300);
        let (w, h) = (1275u32, 1650u32);
        let factor = pixel_factor(f, 150.0);
        assert_eq!(factor, 2.0);
        let o = origin(w, h, factor, 2550, 3300);
        assert_eq!(band_dest(o, factor, 0, w, h), (0, 0, 2550, 3300));
        // Drawn at the dpi asked, nothing changes; at nonsense, the fit's own factor.
        assert_eq!(pixel_factor(f, 300.0), f.factor);
        assert_eq!(pixel_factor(f, f64::NAN), f.factor);
        assert_eq!(pixel_factor(f, 0.0), f.factor);
        // A page shrunk to fit and drawn at less than that: 600 dpi paper, A4 shrunk to 0.94, asked at 282 dpi, drawn at 150.
        let a4 = fit(595.0, 842.0, 5100, 6600, 600);
        let asked = a4.dpi;
        assert!((pixel_factor(a4, asked) - 2.0).abs() < 1e-9);
        assert!((pixel_factor(a4, 150.0) - 2.0 * asked / 150.0).abs() < 1e-9);
    }

    #[test]
    fn a_landscape_page_on_portrait_paper_is_turned() {
        let f = fit(842.0, 595.0, 2480, 3508, 300);
        assert_eq!(f.rotation, 90);
        assert!((f.dpi - 300.0).abs() < 0.1, "{f:?}");
        // On landscape paper it is not.
        assert_eq!(fit(842.0, 595.0, 3508, 2480, 300).rotation, 0);
        // A square page: no reason to turn.
        assert_eq!(fit(500.0, 500.0, 2480, 3508, 300).rotation, 0);
    }

    #[test]
    fn nonsense_sizes_still_give_a_sane_fit() {
        for (w, h, pw, ph, dpi) in [(0.0, 0.0, 0, 0, 0), (-5.0, 1e9, 100, 100, -1), (14400.0, 14400.0, 5100, 6600, 600)] {
            let f = fit(w, h, pw, ph, dpi);
            assert!(f.dpi.is_finite() && f.dpi > 0.0 && f.factor.is_finite() && f.factor >= 1.0 - 1e-9, "{f:?}");
        }
    }

    #[test]
    fn bands_follow_each_other_without_a_gap() {
        let o = origin(2000, 3000, 2.0, 5100, 6600);
        assert_eq!(o, (550, 300));
        let mut covered = 0;
        let mut y = 0;
        for h in [1000u32, 1000, 1000] {
            let (x, top, w, bh) = band_dest(o, 2.0, y, 2000, h);
            assert_eq!((x, w), (550, 4000));
            assert_eq!(top, 300 + covered);
            covered += bh;
            y += h;
        }
        assert_eq!(covered, 6000);
        // A factor that is not a whole number: the bands still meet.
        let f = 1.3333;
        let mut covered = 0;
        let mut y = 0;
        for h in [333u32, 333, 334] {
            let (_, top, _, bh) = band_dest((0, 0), f, y, 100, h);
            assert_eq!(top, covered);
            covered += bh;
            y += h;
        }
        assert_eq!(covered, (1000.0 * f).round() as i32);
        // A page bigger than the paper starts at the corner.
        assert_eq!(origin(9000, 9000, 1.0, 5100, 6600), (0, 0));
    }

    #[test]
    fn page_ranges_are_cut_to_the_document_and_each_page_comes_once() {
        assert_eq!(pages_of(&[], 4), [0, 1, 2, 3]);
        assert_eq!(pages_of(&[(2, 3)], 4), [1, 2]);
        assert_eq!(pages_of(&[(3, 9), (1, 1), (2, 3)], 4), [2, 3, 0, 1]);
        assert_eq!(pages_of(&[(5, 2)], 4), [1, 2, 3]);
        assert_eq!(pages_of(&[(9, 12)], 4), Vec::<u32>::new());
        assert_eq!(pages_of(&[(0, 0)], 4), Vec::<u32>::new());
        assert_eq!(pages_of(&[(1, u32::MAX)], 3), [0, 1, 2]);
    }
}
