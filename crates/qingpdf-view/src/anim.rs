//! The animation clock (2026-10-22): the arithmetic of the short movements of the window. Everything that moves runs on the one timer the reader already has for
//! its messages (`TIMER_ANIM`, a picture every 16 ms) and stops that timer when nothing moves. Plain logic on a clock in milliseconds. When Windows is set to
//! show no animations (`SPI_GETCLIENTAREAANIMATION` is false) every movement takes no time: [`Tween::to`] and [`Levels::set`] jump to where they are going.

/// How long the movements take, in milliseconds.
pub const SLIDE_MS: u64 = 150;
pub const HOVER_MS: u64 = 100;

/// Fast at the start and slow at the end (a cubic), for `t` from 0 to 1.
pub fn ease_out(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// A number that moves from one value to another in a time, eased.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    from: f32,
    to: f32,
    start: u64,
    dur: u64,
}

impl Tween {
    /// A number that stays where it is.
    pub fn at(v: f32) -> Tween {
        Tween { from: v, to: v, start: 0, dur: 0 }
    }

    /// Where it is at `now`.
    pub fn value(&self, now: u64) -> f32 {
        if self.dur == 0 || now >= self.start + self.dur {
            return self.to;
        }
        let t = now.saturating_sub(self.start) as f32 / self.dur as f32;
        self.from + (self.to - self.from) * ease_out(t)
    }

    pub fn moving(&self, now: u64) -> bool {
        self.dur > 0 && now < self.start + self.dur
    }

    /// Go to `target` from where it is now, in `dur` milliseconds (0: at once).
    pub fn to(&mut self, now: u64, target: f32, dur: u64) {
        *self = Tween { from: self.value(now), to: target, start: now, dur };
    }

    pub fn target(&self) -> f32 {
        self.to
    }
}

/// The levels (0 to 1) of a few things that fade in and out, each named by a key: the button the pointer is on fades in and the one it left fades out.
#[derive(Debug)]
pub struct Levels<K> {
    items: Vec<(K, Tween)>,
}

impl<K> Default for Levels<K> {
    fn default() -> Self {
        Levels { items: Vec::new() }
    }
}

impl<K: Copy + PartialEq> Levels<K> {
    /// `key` is on (fades in) or off (fades out), taking `dur` milliseconds (0: at once).
    pub fn set(&mut self, key: K, on: bool, now: u64, dur: u64) {
        let goal = if on { 1.0 } else { 0.0 };
        match self.items.iter_mut().find(|(k, _)| *k == key) {
            Some((_, t)) => t.to(now, goal, dur),
            None if on => {
                let mut t = Tween::at(0.0);
                t.to(now, goal, dur);
                self.items.push((key, t));
            }
            None => {}
        }
    }

    /// How much `key` shows at `now`: 0 to 255.
    pub fn level(&self, key: K, now: u64) -> u8 {
        self.items.iter().find(|(k, _)| *k == key).map_or(0, |(_, t)| (t.value(now) * 255.0).round() as u8)
    }

    /// Is anything still fading (so that the timer must go on)?
    pub fn moving(&self, now: u64) -> bool {
        self.items.iter().any(|(_, t)| t.moving(now))
    }

    /// The keys whose levels are changing at `now`.
    pub fn active(&self, now: u64) -> Vec<K> {
        self.items.iter().filter(|(_, t)| t.moving(now)).map(|(k, _)| *k).collect()
    }

    /// Forget what has faded out.
    pub fn prune(&mut self, now: u64) {
        self.items.retain(|(_, t)| t.moving(now) || t.target() > 0.0);
    }
}

/// The tabs that slide to their new places when a tab is closed: each remaining tab (by its number now) starts `dx` pixels from its place and goes to it.
#[derive(Debug, Default)]
pub struct Slides {
    from: Vec<f32>,
    tween: Option<Tween>,
}

impl Slides {
    /// Tabs at `before` (their lefts) lost the one at `closed`; those after it are now at `after`. Each starts where it was.
    pub fn closed(&mut self, before: &[i64], closed: usize, after: &[i64], now: u64, dur: u64) {
        let old = before.iter().enumerate().filter(|(i, _)| *i != closed).map(|(_, x)| *x);
        self.from = old.zip(after).map(|(o, n)| (o - n) as f32).collect();
        let mut t = Tween::at(1.0);
        t.to(now, 0.0, dur);
        self.tween = Some(t);
    }

    /// How many pixels tab `i` is away from its place at `now`.
    pub fn offset(&self, i: usize, now: u64) -> i64 {
        match (&self.tween, self.from.get(i)) {
            (Some(t), Some(d)) => (d * t.value(now)).round() as i64,
            _ => 0,
        }
    }

    pub fn moving(&self, now: u64) -> bool {
        self.tween.is_some_and(|t| t.moving(now))
    }
}

/// Of the part of the window that is out of date (`dirty`: left, top, right, bottom, in pixels), the part to draw by itself on a picture of its own, if that pays:
/// it is at most a third of the window and lies wholly below the first `keep_rows` rows (the title strip, which the window keys out for Mica, is drawn whole).
pub fn part_to_paint(dirty: (i32, i32, i32, i32), client: (i32, i32), keep_rows: i32) -> Option<(i32, i32, i32, i32)> {
    let (w, h) = client;
    let (l, t, r, b) = (dirty.0.max(0), dirty.1.max(0), dirty.2.min(w), dirty.3.min(h));
    if l >= r || t >= b || t < keep_rows {
        return None;
    }
    let area = i64::from(r - l) * i64::from(b - t);
    (area * 3 <= i64::from(w) * i64::from(h)).then_some((l, t, r, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_part_below_the_title_strip_is_drawn_by_itself_and_anything_else_whole() {
        let client = (1400, 900);
        // The room of a message (a strip across the bottom, a tenth of the window): by itself.
        assert_eq!(part_to_paint((200, 760, 1200, 850), client, 44), Some((200, 760, 1200, 850)));
        // The whole window, and a part that is most of it: whole.
        assert_eq!(part_to_paint((0, 0, 1400, 900), client, 0), None);
        assert_eq!(part_to_paint((0, 100, 1400, 900), client, 0), None);
        // A part that touches the title strip with Mica: whole (the strip is keyed out in one go).
        assert_eq!(part_to_paint((200, 20, 400, 120), client, 44), None);
        assert_eq!(part_to_paint((200, 20, 400, 120), client, 0), Some((200, 20, 400, 120)));
        // A part that sticks out of the window is cut to it; one that is empty or outside is nothing to draw by itself.
        assert_eq!(part_to_paint((1300, 800, 1600, 1000), client, 0), Some((1300, 800, 1400, 900)));
        assert_eq!(part_to_paint((500, 500, 500, 600), client, 0), None);
        assert_eq!(part_to_paint((1500, 0, 1600, 100), client, 0), None);
    }

    #[test]
    fn the_easing_starts_at_zero_ends_at_one_and_never_goes_back() {
        assert_eq!((ease_out(0.0), ease_out(1.0)), (0.0, 1.0));
        assert_eq!((ease_out(-3.0), ease_out(9.0)), (0.0, 1.0));
        let mut last = 0.0;
        for i in 0..=100 {
            let v = ease_out(i as f32 / 100.0);
            assert!(v >= last);
            last = v;
        }
        // Fast at the start: a third of the time takes well over a half of the way.
        assert!(ease_out(0.33) > 0.6);
    }

    #[test]
    fn a_tween_moves_in_its_time_and_a_new_goal_starts_from_where_it_is() {
        let mut t = Tween::at(0.0);
        assert!(!t.moving(0));
        t.to(1000, 1.0, 150);
        assert!(t.moving(1000) && t.moving(1149) && !t.moving(1150));
        assert_eq!(t.value(1000), 0.0);
        assert!(t.value(1075) > 0.5 && t.value(1075) < 1.0);
        assert_eq!(t.value(1150), 1.0);
        // Turned round half-way: it starts from the middle, not from the end.
        let mid = t.value(1060);
        t.to(1060, 0.0, 150);
        assert!((t.value(1060) - mid).abs() < 1e-6);
        assert_eq!(t.value(1210), 0.0);
        // No time at all: it is there.
        t.to(2000, 1.0, 0);
        assert!(!t.moving(2000) && t.value(2000) == 1.0);
    }

    #[test]
    fn a_button_fades_in_while_the_pointer_is_on_it_and_out_when_it_leaves() {
        let mut l: Levels<u8> = Levels::default();
        assert_eq!(l.level(1, 0), 0);
        l.set(1, true, 100, HOVER_MS);
        assert!(l.moving(100) && l.level(1, 100) == 0);
        let half = l.level(1, 150);
        assert!(half > 100 && half < 255, "{half}");
        assert_eq!(l.level(1, 200), 255);
        assert!(!l.moving(200));
        l.set(1, false, 300, HOVER_MS);
        assert!(l.moving(350) && l.level(1, 350) < 200);
        assert_eq!(l.level(1, 400), 0);
        l.prune(400);
        assert!(l.items.is_empty());
        // Off for a key that was never on does nothing; with no animation both are at once.
        l.set(2, false, 400, HOVER_MS);
        assert!(l.items.is_empty());
        l.set(2, true, 500, 0);
        assert!(!l.moving(500) && l.level(2, 500) == 255);
    }

    #[test]
    fn the_tabs_after_a_closed_one_slide_from_where_they_were() {
        let mut s = Slides::default();
        // Three tabs at 0, 100, 200; the middle one is closed; the last is now at 100 (the first stays).
        s.closed(&[0, 100, 200], 1, &[0, 100], 0, SLIDE_MS);
        assert_eq!((s.offset(0, 0), s.offset(1, 0)), (0, 100));
        assert!(s.moving(10) && s.offset(1, 75) > 0 && s.offset(1, 75) < 50);
        assert_eq!((s.offset(1, 150), s.offset(1, 10_000)), (0, 0));
        assert!(!s.moving(150));
        // Tabs that got narrower: every one starts from its old place.
        s.closed(&[0, 120, 240], 2, &[0, 100], 1000, SLIDE_MS);
        assert_eq!((s.offset(0, 1000), s.offset(1, 1000)), (0, 20));
        // Without animation they are in place at once.
        s.closed(&[0, 100, 200], 0, &[0, 100], 0, 0);
        assert_eq!((s.offset(0, 0), s.offset(1, 0)), (0, 0));
        assert!(!s.moving(0));
    }
}
