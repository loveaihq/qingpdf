//! Zoom: a number in thousandths (1000 is 100 percent: a page at its printed size on a screen of 96 dpi, or the
//! same size at the screen's own dpi), the steps between 10 and 800 percent, and the zoom that fits a page.

pub const MIN_ZOOM: u32 = 100;
pub const MAX_ZOOM: u32 = 8000;
pub const ACTUAL_SIZE: u32 = 1000;

/// What the zoom is tied to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomMode {
    /// The page is as wide as the window (re-worked when the window or the page changes).
    FitWidth,
    /// The whole page is seen.
    FitPage,
    Custom,
}

/// The zoom steps of Ctrl+plus and Ctrl+minus, in thousandths.
const STEPS: [u32; 15] = [100, 250, 330, 500, 670, 750, 1000, 1250, 1500, 2000, 3000, 4000, 5000, 6500, 8000];

pub fn clamp(zoom: u32) -> u32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// The next step above `zoom` (the top one if there is none).
pub fn step_in(zoom: u32) -> u32 {
    STEPS.iter().copied().find(|&s| s > zoom).unwrap_or(MAX_ZOOM)
}

/// The next step below `zoom` (the bottom one if there is none).
pub fn step_out(zoom: u32) -> u32 {
    STEPS.iter().rev().copied().find(|&s| s < zoom).unwrap_or(MIN_ZOOM)
}

/// The zoom after `notches` clicks of a wheel (positive: in), 10 percent a click, whole percents, at least one
/// percent a click.
pub fn wheel(zoom: u32, notches: f64) -> u32 {
    if notches == 0.0 || !notches.is_finite() {
        return clamp(zoom);
    }
    let target = f64::from(zoom) * 1.1f64.powf(notches);
    let rounded = (target / 10.0).round() as i64 * 10;
    // At least a percent in the direction asked for.
    let moved = if notches > 0.0 { rounded.max(i64::from(zoom) + 10) } else { rounded.min(i64::from(zoom) - 10) };
    clamp(moved.clamp(0, i64::from(u32::MAX)) as u32)
}

/// Pixels per point at `zoom` on a screen of `dpi` dots per inch.
pub fn scale(zoom: u32, dpi: u32) -> f64 {
    f64::from(zoom) / 1000.0 * f64::from(dpi) / 72.0
}

/// The largest zoom (never above [`MAX_ZOOM`] or below [`MIN_ZOOM`]) at which a page `page_pt` points across fits
/// into `room_px` pixels.
pub fn fit(room_px: f64, page_pt: f64, dpi: u32) -> u32 {
    if !(room_px > 0.0 && page_pt > 0.0) {
        return MIN_ZOOM;
    }
    let zoom = (room_px / page_pt) * 72.0 / f64::from(dpi.max(1)) * 1000.0;
    clamp(zoom.floor().clamp(0.0, f64::from(MAX_ZOOM)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_up_and_down_and_stop_at_the_ends() {
        assert_eq!(step_in(1000), 1250);
        assert_eq!(step_in(1100), 1250);
        assert_eq!(step_out(1000), 750);
        assert_eq!(step_out(1100), 1000);
        assert_eq!(step_in(8000), 8000);
        assert_eq!(step_out(100), 100);
        let mut z = MIN_ZOOM;
        for _ in 0..40 {
            z = step_in(z);
        }
        assert_eq!(z, MAX_ZOOM);
    }

    #[test]
    fn the_wheel_moves_by_a_tenth_and_stays_in_range() {
        assert_eq!(wheel(1000, 1.0), 1100);
        assert_eq!(wheel(1000, -1.0), 910);
        assert_eq!(wheel(MAX_ZOOM, 3.0), MAX_ZOOM);
        assert_eq!(wheel(MIN_ZOOM, -3.0), MIN_ZOOM);
        // A tiny turn still moves.
        assert!(wheel(1000, 0.01) > 1000);
        assert!(wheel(1000, -0.01) < 1000);
        assert_eq!(wheel(1000, 0.0), 1000);
        assert_eq!(wheel(1000, f64::NAN), 1000);
        // Going up and down by the same does not wander far.
        let z = wheel(wheel(1000, 5.0), -5.0);
        assert!((z as i64 - 1000).abs() <= 20, "{z}");
    }

    #[test]
    fn a_page_fits_when_the_zoom_is_the_fit() {
        // A4 across (595 points) in 1000 pixels at 96 dpi: scale = 1000 / 595 pixels a point.
        let z = fit(1000.0, 595.0, 96);
        let px = 595.0 * scale(z, 96);
        assert!(px <= 1000.0 && px > 999.0 * 0.99, "{z} {px}");
        // One more thousandth would not fit.
        assert!(595.0 * scale(z + 1, 96) > 1000.0 - 1e-6 || z + 1 > MAX_ZOOM);
        assert_eq!(fit(0.0, 595.0, 96), MIN_ZOOM);
        assert_eq!(fit(100000.0, 10.0, 96), MAX_ZOOM);
        assert_eq!(fit(10.0, 10000.0, 96), MIN_ZOOM);
    }

    #[test]
    fn a_hundred_percent_is_the_printed_size() {
        // 72 points are an inch: at 96 dpi that is 96 pixels.
        assert!((72.0 * scale(ACTUAL_SIZE, 96) - 96.0).abs() < 1e-9);
        assert!((72.0 * scale(ACTUAL_SIZE, 144) - 144.0).abs() < 1e-9);
    }
}
