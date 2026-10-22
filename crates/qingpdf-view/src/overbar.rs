//! The scroll bars over the pages (2026-10-21; they were the system's before): thin bars (4 pixels) that widen to 8 when the pointer is on them or they are
//! dragged, and fade away a second after the last scroll. Plain arithmetic: where the thumb is for a place of the view, the place of the view for a place of
//! the thumb, what a click on the track does, and how visible and how wide a bar is at a moment. `app/overlay.rs` draws them and takes the mouse.

/// Sizes at 96 dpi: the thin and the wide bar, the room between the bar and the edge of the window, the strip along the edge that wakes the bar when the
/// pointer is on it, the least a thumb is long, and the room kept clear at the ends of the track.
pub const THIN: i32 = 4;
pub const WIDE: i32 = 8;
pub const EDGE: i32 = 2;
pub const ZONE: i32 = 14;
pub const MIN_THUMB: i32 = 32;
pub const END: i32 = 4;

/// How long a bar stays after the last scroll, how long it takes to go, and how long to widen or narrow.
pub const IDLE_MS: u64 = 1000;
pub const FADE_MS: u64 = 250;
pub const GROW_MS: u64 = 120;

/// The thumb of a bar whose track is `track` pixels long, showing `view` of `content` (any one unit) from `pos`: where it starts and how long it is, in
/// pixels along the track. `None` when everything is in view (there is nothing to scroll).
pub fn thumb(track: i64, content: i64, view: i64, pos: i64, min_len: i64) -> Option<(i64, i64)> {
    if track <= 0 || view <= 0 || content <= view {
        return None;
    }
    let len = (track as i128 * view as i128 / content as i128).clamp(i128::from(min_len.min(track)), i128::from(track)) as i64;
    let range = content - view;
    let start = (i128::from(track - len) * i128::from(pos.clamp(0, range)) / i128::from(range)) as i64;
    Some((start, len))
}

/// The place of the view (from 0 to `content - view`) the thumb of length `len` stands for when it starts at `start` pixels along a track of `track`.
pub fn pos_at(track: i64, content: i64, view: i64, len: i64, start: i64) -> i64 {
    let (room, range) = (track - len, content - view);
    if room <= 0 || range <= 0 {
        return 0;
    }
    (i128::from(start.clamp(0, room)) * i128::from(range) / i128::from(room)) as i64
}

/// What a click on the track does: above the thumb is a page back (-1), below it a page on (1), on it nothing (0).
pub fn track_click(thumb: (i64, i64), at: i64) -> i32 {
    if at < thumb.0 {
        -1
    } else if at >= thumb.0 + thumb.1 {
        1
    } else {
        0
    }
}

/// What a bar is doing: when it was last woken (a scroll, the pointer on it), whether the pointer is on it or it is dragged, and when that last changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vis {
    pub woken: Option<u64>,
    pub hover: bool,
    pub drag: bool,
    pub changed: u64,
}

impl Vis {
    /// The view was scrolled (or the bar was asked to show).
    pub fn wake(&mut self, now: u64) {
        self.woken = Some(now);
    }

    pub fn set_hover(&mut self, on: bool, now: u64) {
        if self.hover != on {
            self.hover = on;
            self.changed = now;
        }
        // (A bar stays a second after the pointer has left it, too.)
        self.woken = Some(now);
    }

    pub fn set_drag(&mut self, on: bool, now: u64) {
        if self.drag != on {
            self.drag = on;
            self.changed = now;
        }
        self.woken = Some(now);
    }

    /// How opaque the bar is, 0 to 255.
    pub fn alpha(&self, now: u64) -> u8 {
        if self.hover || self.drag {
            return 255;
        }
        let Some(age) = self.woken.map(|t| now.saturating_sub(t)) else { return 0 };
        if age < IDLE_MS {
            255
        } else if age < IDLE_MS + FADE_MS {
            (255 - (age - IDLE_MS) * 255 / FADE_MS) as u8
        } else {
            0
        }
    }

    /// How wide the bar is, in pixels, between `thin` and `wide`.
    pub fn width(&self, now: u64, thin: i32, wide: i32) -> i32 {
        let k = (now.saturating_sub(self.changed) * 256 / GROW_MS).min(256) as i64;
        let level = if self.hover || self.drag { k } else { 256 - k };
        thin + ((i64::from(wide - thin) * level) / 256) as i32
    }

    /// Is something about the bar changing at `now` (so that the clock must go on)?
    pub fn moving(&self, now: u64) -> bool {
        if self.hover || self.drag {
            return now.saturating_sub(self.changed) < GROW_MS;
        }
        now.saturating_sub(self.changed) < GROW_MS || self.woken.is_some_and(|t| now.saturating_sub(t) < IDLE_MS + FADE_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_is_as_long_as_the_part_in_view_and_goes_from_one_end_to_the_other() {
        // A track of 600 pixels, a view of a fifth of the content: a thumb of 120.
        assert_eq!(thumb(600, 5000, 1000, 0, 32), Some((0, 120)));
        assert_eq!(thumb(600, 5000, 1000, 4000, 32), Some((480, 120)));
        assert_eq!(thumb(600, 5000, 1000, 2000, 32), Some((240, 120)));
        // A place beyond the ends is held to them.
        assert_eq!(thumb(600, 5000, 1000, -50, 32), Some((0, 120)));
        assert_eq!(thumb(600, 5000, 1000, 99999, 32), Some((480, 120)));
        // A long document has a thumb of the least length; the whole of it is still reached.
        assert_eq!(thumb(600, 1_000_000, 1000, 0, 32), Some((0, 32)));
        assert_eq!(thumb(600, 1_000_000, 1000, 999_000, 32), Some((568, 32)));
        // Nothing to scroll: no thumb.
        assert_eq!(thumb(600, 1000, 1000, 0, 32), None);
        assert_eq!(thumb(600, 800, 1000, 0, 32), None);
        assert_eq!(thumb(0, 5000, 1000, 0, 32), None);
        assert_eq!(thumb(600, 5000, 0, 0, 32), None);
        // A track shorter than the least thumb: the thumb fills it.
        assert_eq!(thumb(20, 5000, 1000, 0, 32), Some((0, 20)));
        // Numbers that would overflow 64 bits are fine.
        assert!(thumb(i64::MAX / 2, i64::MAX, 1000, i64::MAX / 2, 32).is_some());
    }

    #[test]
    fn dragging_the_thumb_gives_back_the_place_that_put_it_there() {
        let (track, content, view) = (600, 5000, 1000);
        for pos in [0, 1, 777, 2000, 3999, 4000] {
            let (start, len) = thumb(track, content, view, pos, 32).unwrap();
            let back = pos_at(track, content, view, len, start);
            // (The thumb moves in whole pixels: the place is the same to within a pixel's worth of it.)
            assert!((back - pos).abs() <= (content - view) / (track - len) + 1, "{pos} -> {start} -> {back}");
        }
        assert_eq!(pos_at(track, content, view, 120, -10), 0);
        assert_eq!(pos_at(track, content, view, 120, 10_000), 4000);
        assert_eq!(pos_at(100, 5000, 1000, 100, 50), 0, "no room to move");
        assert_eq!(pos_at(600, 800, 1000, 120, 50), 0, "nothing to scroll");
    }

    #[test]
    fn a_click_on_the_track_goes_a_page_towards_it() {
        assert_eq!(track_click((100, 50), 40), -1);
        assert_eq!(track_click((100, 50), 99), -1);
        assert_eq!(track_click((100, 50), 100), 0);
        assert_eq!(track_click((100, 50), 149), 0);
        assert_eq!(track_click((100, 50), 150), 1);
    }

    #[test]
    fn a_bar_shows_for_a_second_after_a_scroll_then_fades_and_stays_under_the_pointer() {
        let mut v = Vis::default();
        assert_eq!(v.alpha(5000), 0, "nothing woke it");
        v.wake(5000);
        assert_eq!(v.alpha(5000), 255);
        assert_eq!(v.alpha(5000 + IDLE_MS - 1), 255);
        let mid = v.alpha(5000 + IDLE_MS + FADE_MS / 2);
        assert!(mid > 100 && mid < 160, "{mid}");
        assert_eq!(v.alpha(5000 + IDLE_MS + FADE_MS), 0);
        assert!(v.moving(5000 + IDLE_MS) && !v.moving(5000 + IDLE_MS + FADE_MS + GROW_MS));
        // A scroll again wakes it again; the pointer on it keeps it; a drag keeps it.
        v.wake(9000);
        assert_eq!(v.alpha(9100), 255);
        v.set_hover(true, 20_000);
        assert_eq!(v.alpha(30_000), 255);
        v.set_hover(false, 30_000);
        assert_eq!(v.alpha(30_000 + IDLE_MS - 1), 255, "it stays a second after the pointer has left");
        v.set_drag(true, 40_000);
        assert_eq!(v.alpha(99_000), 255);
        v.set_drag(false, 99_000);
        assert_eq!(v.alpha(99_000 + IDLE_MS + FADE_MS), 0);
        // A clock that went back is not a reason to fail.
        assert_eq!(Vis { woken: Some(10_000), ..Vis::default() }.alpha(5000), 255);
    }

    #[test]
    fn a_bar_widens_when_the_pointer_comes_and_narrows_when_it_goes() {
        let mut v = Vis::default();
        assert_eq!(v.width(5000, THIN, WIDE), THIN);
        v.set_hover(true, 5000);
        assert_eq!(v.width(5000, THIN, WIDE), THIN);
        let mid = v.width(5000 + GROW_MS / 2, THIN, WIDE);
        assert!(mid > THIN && mid < WIDE, "{mid}");
        assert_eq!(v.width(5000 + GROW_MS, THIN, WIDE), WIDE);
        assert_eq!(v.width(99_000, THIN, WIDE), WIDE);
        v.set_hover(false, 99_000);
        assert_eq!(v.width(99_000, THIN, WIDE), WIDE);
        assert_eq!(v.width(99_000 + GROW_MS, THIN, WIDE), THIN);
        v.set_drag(true, 100_000);
        assert_eq!(v.width(100_000 + GROW_MS, THIN, WIDE), WIDE);
        assert!(v.moving(100_000 + 5) && !v.moving(100_000 + GROW_MS));
    }
}
