//! The short movements of the window (2026-10-22): the fill under a button or a tab the pointer is on fades in and out, the sidebar slides in and out, the tabs
//! after a closed one slide into its place. All of it is pictures on the clock the little messages already use (`TIMER_ANIM`, every 16 ms); nothing waits for a
//! movement to end: the pages are laid out at once for where the sidebar is going, and a key or a click is heard as ever. The arithmetic is in `anim.rs`; when
//! Windows is set to show no animations (`Event::Motion(false)`) each movement takes no time at all.

use super::overlay::Hot;
use super::*;
use crate::anim::{self, Levels, Slides, Tween};

pub(super) struct Fx {
    /// Windows allows animations.
    pub on: bool,
    /// How much of its fill each button or tab the pointer is on (or has just left) shows.
    pub hover: Levels<Hot>,
    /// How far in the sidebar is (1: all of it), and, while it slides out after being closed, the width the window gave it (its pages are laid out for none).
    pub side: Tween,
    pub ghost: i64,
    /// The tabs after a closed one.
    pub slides: Slides,
}

impl Fx {
    pub(super) fn new() -> Fx {
        Fx { on: true, hover: Levels::default(), side: Tween::at(1.0), ghost: 0, slides: Slides::default() }
    }
}

impl App {
    /// Windows says whether it shows animations.
    pub(super) fn set_motion(&mut self, on: bool) -> Vec<Action> {
        self.fx.on = on;
        self.toasts.still = !on;
        Vec::new()
    }

    fn move_ms(&self, ms: u64) -> u64 {
        if self.fx.on { ms } else { 0 }
    }

    /// The pointer went from `was` to `hot`: the one fades out and the other in.
    pub(super) fn hover_moved(&mut self, was: Option<Hot>, hot: Option<Hot>) -> Vec<Action> {
        let (now, ms) = (self.now_ms(), self.move_ms(anim::HOVER_MS));
        if let Some(h) = was {
            self.fx.hover.set(h, false, now, ms);
        }
        if let Some(h) = hot {
            self.fx.hover.set(h, true, now, ms);
        }
        if self.fx.hover.moving(now) { self.start_anim() } else { Vec::new() }
    }

    /// How much of its hover fill a button shows (0 to 255); a pressed one shows all of it.
    pub(super) fn hover_level(&self, hot: Hot) -> u8 {
        if self.is_pressed(hot) { 255 } else { self.fx.hover.level(hot, self.now_ms()) }
    }

    /// Does anything here still move at `now` (so that the clock must go on)?
    pub(super) fn motion_busy(&self, now: u64) -> bool {
        self.fx.hover.moving(now) || self.fx.side.moving(now) || self.fx.slides.moving(now) || self.fx.ghost > 0
    }

    /// A tick of the clock: what has faded out is forgotten, a sidebar that has slid out is gone (the actions put its tree away).
    pub(super) fn motion_tick(&mut self, now: u64) -> Vec<Action> {
        self.fx.hover.prune(now);
        let sliding = self.fx.side.moving(now);
        if self.fx.ghost > 0 && !sliding {
            self.fx.ghost = 0;
            self.fx.side = Tween::at(1.0);
            self.sync_thumbs();
            self.cancel_thumbs();
            return self.chrome();
        }
        if sliding { self.chrome() } else { Vec::new() }
    }

    /// The sidebar is about to be shown or hidden by the person: it slides. `was` is the width it had.
    pub(super) fn side_slide(&mut self, was: i64) {
        let (now, ms) = (self.now_ms(), self.move_ms(anim::SLIDE_MS));
        let will = self.sidebar_width();
        self.fx.ghost = 0;
        if ms == 0 || self.bare || self.bench.is_some() {
            self.fx.side = Tween::at(1.0);
        } else if was > 0 && will == 0 {
            self.fx.ghost = was;
            self.fx.side.to(now, 0.0, ms);
        } else if was == 0 && will > 0 {
            self.fx.side = Tween::at(0.0);
            self.fx.side.to(now, 1.0, ms);
        }
    }

    /// How far left the sidebar's card is from its place now (pixels, 0 or less).
    pub(super) fn side_dx(&self) -> i64 {
        let v = self.fx.side.value(self.now_ms());
        -(((1.0 - v) * (self.side_x() as f32)).round() as i64)
    }

    /// A tab was closed: those after it slide from where they were. `before` is where the tabs' lefts were, `closed` the one that went.
    pub(super) fn slide_tabs(&mut self, before: &[i64], closed: usize) -> Vec<Action> {
        let (now, ms) = (self.now_ms(), self.move_ms(anim::SLIDE_MS));
        let after = self.strip().xs;
        self.fx.slides.closed(before, closed, &after, now, ms);
        if self.fx.slides.moving(now) { self.start_anim() } else { Vec::new() }
    }

    /// How far tab `i` is from its place now.
    pub(super) fn tab_offset(&self, i: usize) -> i64 {
        self.fx.slides.offset(i, self.now_ms())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::sidebar::tests::{Rec, letter, open_pages};
    use super::*;
    use crate::ui::vk;

    fn window(name: &str) -> App {
        let mut app = open_pages(name, &letter(4), true);
        app.set_bare(false);
        app.event(Event::Size { w: 1200, h: 900 });
        app
    }

    fn f4(app: &mut App) -> Vec<Action> {
        app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false })
    }

    #[test]
    fn turning_pages_starts_no_clock_in_a_window_with_nothing_over_the_pages_and_an_idle_window_has_none_running() {
        // The window of the speed test (bare: no bars, pill, messages or chrome) turns pages with no timer at all, so nothing is drawn but what the turn needs.
        let rest = |app: &mut App| {
            // (Opening the file may have said something: the clock goes when the next tick finds nothing moving.)
            app.toasts.clear();
            app.event(Event::Timer(TIMER_ANIM));
            let now = app.now_ms();
            assert!(!app.anim_on && !app.motion_busy(now), "on {} toasts {} bars {} {} pill {} motion {}", app.anim_on, app.toasts.busy(), app.vis_v.moving(now), app.vis_h.moving(now), crate::pill::moving(now, app.pill_scrolled), app.motion_busy(now));
        };
        let mut app = open_pages("idle", &letter(6), false);
        rest(&mut app);
        for _ in 0..4 {
            let turned = app.event(Event::Command(CMD_NEXT_PAGE));
            assert!(!turned.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_ANIM, .. })), "{turned:?}");
        }
        assert!(!app.anim_on && !app.motion_busy(app.now_ms()));
        // A window at rest (with its chrome) has no clock going either, and the pointer moving over nothing starts none.
        let mut app = window("rest");
        // (The bars and the pill show for a moment after the pages were laid out; when they have gone the next tick takes the clock away.)
        std::thread::sleep(Duration::from_millis(1400));
        rest(&mut app);
        let moved = app.event(Event::Hover { x: 700, y: 400 });
        assert!(!moved.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_ANIM, .. })) && !app.anim_on);
    }

    #[test]
    fn the_sidebar_slides_out_when_it_is_hidden_and_is_drawn_while_it_goes_and_the_pages_are_laid_out_at_once() {
        let mut app = window("out");
        assert!(app.view_x > 0 && app.side_dx() == 0 && app.fx.ghost == 0);
        let width = app.view_x;
        let hidden = f4(&mut app);
        // The pages have their room at once; the card goes on being drawn, to the left of its place and further left as time goes.
        assert_eq!((app.view_x, app.fx.ghost), (0, width));
        assert!(hidden.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_ANIM, .. })) || app.anim_on);
        // (At the very start it is still at its place; a moment later it is away from it.)
        std::thread::sleep(Duration::from_millis(30));
        let first = app.side_dx();
        assert!(first < 0, "{first}");
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.texts.iter().any(|(x, _, s)| s == "Pages" && *x < 1200), "the card is drawn");
        std::thread::sleep(Duration::from_millis(60));
        assert!(app.side_dx() < first, "it goes on");
        // When the time is up the next tick takes it away: nothing of it is drawn, its tree is put away, and a sidebar shown later is not left out of place.
        std::thread::sleep(Duration::from_millis(150));
        let tick = app.event(Event::Timer(TIMER_ANIM));
        assert_eq!((app.fx.ghost, app.side_dx()), (0, 0));
        assert!(tick.iter().any(|a| matches!(a, Action::Sidebar(r) if r.w == 0)));
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(!rec.texts.iter().any(|(_, _, s)| s == "Pages"));
        assert!(!app.motion_busy(app.now_ms()));
    }

    #[test]
    fn the_sidebar_slides_in_from_the_left_when_it_is_shown_and_toggling_in_the_middle_turns_it_round() {
        let mut app = window("in");
        f4(&mut app);
        std::thread::sleep(Duration::from_millis(200));
        app.event(Event::Timer(TIMER_ANIM));
        f4(&mut app);
        assert!(app.view_x > 0 && app.fx.ghost == 0);
        let start = app.side_dx();
        assert!(start < -(app.card_w() / 2), "it starts out of the window: {start}");
        std::thread::sleep(Duration::from_millis(60));
        let later = app.side_dx();
        assert!(later > start && later < 0, "{start} {later}");
        // Hidden in the middle of coming: it goes back out from where it is.
        f4(&mut app);
        assert!(app.fx.ghost > 0 && app.side_dx() <= later + 2);
        std::thread::sleep(Duration::from_millis(200));
        app.event(Event::Timer(TIMER_ANIM));
        assert_eq!((app.fx.ghost, app.side_dx(), app.view_x), (0, 0, 0));
    }

    #[test]
    fn with_no_animations_the_sidebar_comes_and_goes_at_once_and_so_does_a_message() {
        let mut app = window("still");
        app.event(Event::Motion(false));
        f4(&mut app);
        assert_eq!((app.view_x, app.fx.ghost, app.side_dx()), (0, 0, 0));
        f4(&mut app);
        assert!(app.view_x > 0 && app.side_dx() == 0 && !app.fx.side.moving(app.now_ms()));
        // A message is at full strength the moment it is said, and a button is at full strength the moment the pointer is on it.
        app.say("hello".to_string());
        assert_eq!(app.toasts.view(app.now_ms()).map(|v| (v.alpha, v.lift)), Some((255, 0)));
        app.set_hot(Some(overlay::Hot::Cmd(1)));
        assert_eq!(app.hover_level(overlay::Hot::Cmd(1)), 255);
    }

    #[test]
    fn the_fill_under_a_button_fades_in_while_the_pointer_is_on_it_and_out_when_it_leaves() {
        let mut app = window("fade");
        let button = overlay::Hot::Cmd(1);
        let on = app.set_hot(Some(button));
        assert!(on.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_ANIM, .. })) || app.anim_on);
        assert!(app.hover_level(button) < 255, "it begins faint");
        std::thread::sleep(Duration::from_millis(40));
        let half = app.hover_level(button);
        assert!(half > 0, "{half}");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(app.hover_level(button), 255);
        // Another button takes the pointer: the first fades out as the second fades in, and the clock stops when both are done.
        app.set_hot(Some(overlay::Hot::Cmd(2)));
        assert!(app.hover_level(button) > 0 && app.hover_level(overlay::Hot::Cmd(2)) < 255);
        std::thread::sleep(Duration::from_millis(160));
        app.event(Event::Timer(TIMER_ANIM));
        assert_eq!((app.hover_level(button), app.hover_level(overlay::Hot::Cmd(2))), (0, 255));
        assert!(!app.fx.hover.moving(app.now_ms()));
        // What is drawn goes with it: the fill under the button the pointer is on is in the picture.
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.fills.iter().any(|(_, c)| *c == app.pal.status_fg));
    }
}
