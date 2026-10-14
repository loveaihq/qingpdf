//! Where the window goes when it starts: the place and size it had when it was closed, put back where it can be seen. The
//! window gives the work areas of the screens (the screen less the task bar), the first being the main one.

use crate::ui::Rect;

/// The smallest window that is restored (smaller than that is the work of a damaged settings file).
pub const MIN_W: i32 = 480;
pub const MIN_H: i32 = 320;
/// Of a window that is partly off the screens, this much (in pixels, across and down) has to be on one screen for it to stay where it is.
const MIN_VISIBLE_W: i32 = 160;
const MIN_VISIBLE_H: i32 = 80;

/// The place of the window as it is when it is not maximized (`x`, `y`, `w`, `h` of the whole window on the screens), and whether it
/// is maximized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
}

impl Placement {
    /// The settings file's `x,y,w,h,m` (m is 1 for maximized).
    pub fn to_text(self) -> String {
        format!("{},{},{},{},{}", self.x, self.y, self.w, self.h, i32::from(self.maximized))
    }

    /// The inverse of [`Placement::to_text`]; `None` for anything else, and for numbers that no window has.
    pub fn parse(s: &str) -> Option<Placement> {
        let mut it = s.split(',').map(|p| p.trim().parse::<i32>().ok());
        let (x, y, w, h, m) = (it.next()??, it.next()??, it.next()??, it.next()??, it.next()??);
        if it.next().is_some() || !(-32768..=32767).contains(&x) || !(-32768..=32767).contains(&y) || !(MIN_W..=20000).contains(&w) || !(MIN_H..=20000).contains(&h) || !(0..=1).contains(&m) {
            return None;
        }
        Some(Placement { x, y, w, h, maximized: m == 1 })
    }
}

fn overlap(a: Rect, b: Rect) -> (i32, i32) {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    (w.max(0), h.max(0))
}

/// `saved` put where it can be seen: on the screen it overlaps most, no bigger than that screen's work area and moved so that all of
/// it is on it; a window that is (almost) off every screen (a monitor that is not there any more) goes to the middle of the first
/// screen. `None` when there are no screens to put it on.
pub fn restore(saved: Placement, work_areas: &[Rect]) -> Option<Placement> {
    let first = *work_areas.first()?;
    let me = Rect { x: saved.x, y: saved.y, w: saved.w, h: saved.h };
    let best = work_areas
        .iter()
        .copied()
        .filter(|a| a.w > 0 && a.h > 0)
        .max_by_key(|&a| {
            let (w, h) = overlap(me, a);
            i64::from(w) * i64::from(h)
        })
        .unwrap_or(first);
    let (ow, oh) = overlap(me, best);
    let on_screen = ow >= MIN_VISIBLE_W.min(saved.w) && oh >= MIN_VISIBLE_H.min(saved.h);
    let area = if on_screen { best } else { first };
    let w = saved.w.min(area.w).max(MIN_W.min(area.w));
    let h = saved.h.min(area.h).max(MIN_H.min(area.h));
    let (x, y) = if on_screen {
        (saved.x.clamp(area.x, (area.x + area.w - w).max(area.x)), saved.y.clamp(area.y, (area.y + area.h - h).max(area.y)))
    } else {
        (area.x + (area.w - w) / 2, area.y + (area.h - h) / 2)
    };
    Some(Placement { x, y, w, h, maximized: saved.maximized })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect { x: 0, y: 0, w: 1920, h: 1040 };
    const RIGHT: Rect = Rect { x: 1920, y: 0, w: 2560, h: 1400 };

    fn at(x: i32, y: i32, w: i32, h: i32) -> Placement {
        Placement { x, y, w, h, maximized: false }
    }

    #[test]
    fn a_window_that_fits_stays_where_it_was() {
        assert_eq!(restore(at(100, 80, 1200, 800), &[MAIN]), Some(at(100, 80, 1200, 800)));
        assert_eq!(restore(at(2000, 100, 1000, 700), &[MAIN, RIGHT]), Some(at(2000, 100, 1000, 700)));
        let mut m = at(100, 80, 1200, 800);
        m.maximized = true;
        assert_eq!(restore(m, &[MAIN]), Some(m));
    }

    #[test]
    fn a_window_that_is_too_big_is_made_to_fit_its_screen() {
        assert_eq!(restore(at(0, 0, 3000, 2000), &[MAIN]), Some(at(0, 0, 1920, 1040)));
        // The screen it overlaps most is the one it is held to.
        assert_eq!(restore(at(1800, 0, 3000, 1000), &[MAIN, RIGHT]), Some(at(1920, 0, 2560, 1000)));
    }

    #[test]
    fn a_window_partly_off_the_screen_is_moved_in() {
        assert_eq!(restore(at(1000, 700, 1200, 800), &[MAIN]), Some(at(720, 240, 1200, 800)));
        assert_eq!(restore(at(-300, -50, 1200, 800), &[MAIN]), Some(at(0, 0, 1200, 800)));
    }

    #[test]
    fn a_window_that_was_on_a_monitor_that_is_gone_goes_to_the_middle_of_the_first() {
        assert_eq!(restore(at(2400, 200, 1000, 700), &[MAIN]), Some(at(460, 170, 1000, 700)));
        assert_eq!(restore(at(-5000, -5000, 1000, 700), &[MAIN]), Some(at(460, 170, 1000, 700)));
        // Only a sliver of it on the screen is as good as none.
        assert_eq!(restore(at(1900, 200, 1000, 700), &[MAIN]), Some(at(460, 170, 1000, 700)));
        assert_eq!(restore(at(0, 0, 1000, 700), &[]), None);
    }

    #[test]
    fn the_settings_text_is_read_back_and_nonsense_is_not() {
        let p = at(-1200, 40, 1000, 700);
        assert_eq!(Placement::parse(&p.to_text()), Some(p));
        assert_eq!(Placement::parse("1,2,1000,700,1").map(|p| p.maximized), Some(true));
        for bad in ["", "1,2,3", "1,2,1000,700", "1,2,1000,700,2", "a,2,1000,700,0", "1,2,10,700,0", "1,2,1000,700,0,5", "99999,2,1000,700,0", "1,2,1000,99999,0"] {
            assert_eq!(Placement::parse(bad), None, "{bad}");
        }
    }
}
