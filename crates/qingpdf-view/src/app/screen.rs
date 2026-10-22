//! Full screen (F11) and the presentation (F5) (2026-10-20). In full screen the window has no frame, no menu, no toolbar, no strip of tabs, no
//! sidebar and no status bar, and its page area is the whole of the monitor it is on (the window does that: `Action::FullScreen`; the reader
//! only stops reserving room for the parts that are gone). The presentation is full screen with one page at a time, fitted to the screen on a
//! black ground: the layout is one page to a row with no margin and a gap as high as the screen, so that the pages next to the one shown are
//! never in view, and the view is always at the top of a page. The scheduler's ring is what draws the next page and the one before while this
//! one is read, so a page turn finds its picture in the cache.

use super::*;

/// The view as it was before the presentation (the zoom the person had; the layout, the margins and the gaps are put back by the layout).
#[derive(Clone, Copy)]
pub(super) struct Saved {
    pub(super) mode: ZoomMode,
    pub(super) zoom: u32,
}

pub(super) struct Present {
    pub(super) saved: Saved,
    /// The window was full screen before the presentation, and stays so after it.
    was_full: bool,
    /// The pointer is hidden (it was still for a while), and where it was then: it is shown again when it is somewhere else.
    hidden_at: Option<(i64, i64)>,
    /// What the wheel has turned that is not yet a page.
    wheel: i32,
}

impl App {
    /// F11 and View > Full screen.
    pub(super) fn toggle_full(&mut self) -> Vec<Action> {
        if self.present.is_some() {
            return self.end_present(true);
        }
        self.set_full(!self.full)
    }

    /// The window goes full screen or comes back: the toolbar, the strips and the sidebar are given up or taken again, and the window is asked
    /// to change (it tells the new size of the page area, and the pages are laid out again then).
    fn set_full(&mut self, on: bool) -> Vec<Action> {
        if self.full == on {
            return Vec::new();
        }
        self.full = on;
        self.menus_dirty = true;
        // (The find box of the toolbar is gone, or comes: what is open starts again with the other.)
        let mut actions = if self.find_open { self.close_find() } else { Vec::new() };
        actions.insert(0, Action::FullScreen(on));
        actions
    }

    /// Esc: when nothing else wanted it, full screen is over.
    pub(super) fn escape_full(&mut self) -> Vec<Action> {
        if self.full { self.set_full(false) } else { Vec::new() }
    }

    /// Where the view is for page `page` of the presentation: the page in the middle of the screen, from top to bottom (one as high as the screen is at
    /// the top). The view is outside the document for a page shorter than the screen; nothing else is in it, the gaps are as high as the screen.
    pub(super) fn presented_at(&self, page: usize) -> i64 {
        self.doc.as_ref().map_or(0, |d| d.layout.scroll_to_page(page, (self.view_h - d.layout.size_px(page).1) / 2))
    }

    /// F5 and View > Presentation: from the page in view.
    pub(super) fn start_present(&mut self) -> Vec<Action> {
        if self.doc.is_none() || self.present.is_some() {
            return Vec::new();
        }
        self.present = Some(Present { saved: Saved { mode: self.mode, zoom: self.zoom }, was_full: self.full, hidden_at: None, wheel: 0 });
        self.selection = None;
        self.press = None;
        // (The speed test has the window it was given.)
        let mut actions = if self.bench.is_some() { Vec::new() } else { self.set_full(true) };
        self.relayout(0, 0, None);
        actions.extend(self.after_view_change(false));
        actions.push(Action::SetTimer { id: TIMER_CURSOR, ms: CURSOR_MS });
        actions
    }

    /// Esc ends the presentation at the page shown (and full screen with it, unless the window was full screen before; F11 ends both).
    pub(super) fn end_present(&mut self, and_full: bool) -> Vec<Action> {
        let page = self.current_page();
        let Some(p) = self.present.take() else { return Vec::new() };
        (self.mode, self.zoom) = (p.saved.mode, p.saved.zoom);
        let mut actions = vec![Action::KillTimer(TIMER_CURSOR)];
        if self.full && (and_full || !p.was_full) {
            // The page is put at the top again when the window has its own size back (the old size is not known until then).
            self.restore_page = Some(page);
            actions.extend(self.set_full(false));
        }
        self.relayout(0, 0, None);
        actions.extend(self.go_to_page(page));
        actions
    }

    /// Show page `page` (a link, Home, End), fitted to the screen.
    pub(super) fn present_goto(&mut self, page: usize) -> Vec<Action> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let page = page.min(doc.layout.len().saturating_sub(1));
        // A page of another size has its own fit; one the size of the last is the same picture at the same scale, which the ring has drawn.
        let same = doc.layout.size_pt(page) == doc.layout.size_pt(self.current_page());
        self.scroll_y = self.presented_at(page);
        if !same {
            self.relayout(0, 0, None);
        }
        self.after_view_change(false)
    }

    /// One page on (`by` > 0) or back; at either end it stays.
    pub(super) fn present_turn(&mut self, by: isize) -> Vec<Action> {
        let here = self.current_page();
        let to = here.saturating_add_signed(by).min(self.page_count().saturating_sub(1));
        if to == here { Vec::new() } else { self.present_goto(to) }
    }

    /// A key in the presentation: Space, Enter, PgDn and the arrows right and down are on, Backspace, PgUp and the arrows left and up are back.
    pub(super) fn present_key(&mut self, key: u32, shift: bool) -> Vec<Action> {
        match key {
            vk::ESCAPE => self.end_present(false),
            vk::F11 => self.end_present(true),
            vk::SPACE if shift => self.present_turn(-1),
            vk::SPACE | vk::RETURN | vk::NEXT | vk::RIGHT | vk::DOWN => self.present_turn(1),
            vk::BACK | vk::PRIOR | vk::LEFT | vk::UP => self.present_turn(-1),
            vk::HOME => self.present_goto(0),
            vk::END => self.present_goto(usize::MAX),
            _ => Vec::new(),
        }
    }

    /// The left button went down in the presentation: a link is followed, anywhere else is the next page.
    pub(super) fn present_click(&mut self, x: i64, y: i64) -> Vec<Action> {
        let link = self.page_at_point(x, y).and_then(|(page, px, py)| {
            let (xp, yp) = self.doc.as_ref()?.layout.page_map(page)?.to_pt(px, py);
            Some((page as u32, self.link_at(page, xp, yp)?))
        });
        match link {
            Some((page, index)) => self.activate_link(page, index),
            None => self.present_turn(1),
        }
    }

    /// The wheel: a page for each click, up is back.
    pub(super) fn present_wheel(&mut self, delta: i32) -> Vec<Action> {
        let Some(p) = self.present.as_mut() else { return Vec::new() };
        p.wheel += delta;
        let clicks = p.wheel / 120;
        p.wheel -= clicks * 120;
        if clicks == 0 { Vec::new() } else { self.present_turn(-clicks as isize) }
    }

    /// The pointer moved: it is shown, and hidden again if it is still for two seconds.
    pub(super) fn present_pointer(&mut self, x: i64, y: i64) -> Vec<Action> {
        self.mouse = (x, y);
        match self.present.as_mut() {
            Some(p) => {
                p.hidden_at = None;
                vec![Action::SetTimer { id: TIMER_CURSOR, ms: CURSOR_MS }]
            }
            None => Vec::new(),
        }
    }

    /// Two seconds without the pointer moving: it is hidden (until it moves).
    pub(super) fn present_idle(&mut self) -> Vec<Action> {
        // (The timer may have been queued before Esc ended the presentation: the pointer of the window it came back to is not hidden.)
        let at = self.mouse;
        let Some(p) = self.present.as_mut() else { return Vec::new() };
        p.hidden_at = Some(at);
        vec![Action::KillTimer(TIMER_CURSOR), Action::HideCursor]
    }

    /// Is the pointer hidden at this place?
    pub(super) fn pointer_hidden_at(&self, x: i64, y: i64) -> bool {
        self.present.as_ref().is_some_and(|p| p.hidden_at == Some((x, y)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::{Rec, open_pages, pump};
    use super::*;

    fn key(app: &mut App, vk: u32) -> Vec<Action> {
        app.event(Event::Key { vk, ctrl: false, shift: false })
    }

    /// The pages as the window shows them: its page area is this big.
    fn open(name: &str, sizes: &[(u32, u32, u32)], thumbs_tab: bool) -> App {
        let mut app = open_pages(name, sizes, thumbs_tab);
        app.event(Event::Size { w: 1600, h: 900 });
        pump(&mut app, "the pages", |a| a.visible_ready());
        app
    }

    #[test]
    fn full_screen_gives_up_the_sidebar_the_command_bar_and_the_strip_and_esc_gives_them_back() {
        let mut app = open("full", &[(612, 792, 0); 5], true);
        app.bare = false;
        app.event(Event::Size { w: 1600, h: 900 });
        assert!(app.view_x > 0 && app.toolbar_visible() && app.bar_h() == 84);
        let actions = key(&mut app, vk::F11);
        assert!(actions.iter().any(|a| matches!(a, Action::FullScreen(true))));
        assert!(actions.iter().any(|a| matches!(a, Action::SearchBox(None))), "the search box of the command bar goes");
        assert_eq!(app.bar_h(), 0);
        // The window tells the new size of the page area: the whole of the monitor, with no sidebar.
        app.event(Event::Size { w: 1920, h: 1080 });
        assert!((app.view_x, app.view_w, app.view_h, app.bar_h()) == (0, 1920, 1080, 0), "{} {}", app.view_x, app.view_w);
        // The menu says so, and F11 again is the way back; so is Esc.
        let menus = app.menus();
        let view = menus.iter().find(|m| m.title.contains("View")).expect("the View menu");
        assert!(view.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, checked: true, .. } if *id == CMD_FULLSCREEN)));
        let back = key(&mut app, vk::ESCAPE);
        assert!(back.iter().any(|a| matches!(a, Action::FullScreen(false))) && !app.full);
        app.event(Event::Size { w: 1600, h: 900 });
        assert!(app.view_x > 0 && app.toolbar_visible());
        assert!(key(&mut app, vk::F11).iter().any(|a| matches!(a, Action::FullScreen(true))));
        assert!(key(&mut app, vk::F11).iter().any(|a| matches!(a, Action::FullScreen(false))));
        // Esc does not leave while something else wants it: the find box is closed first.
        key(&mut app, vk::F11);
        app.event(Event::Command(CMD_FIND));
        assert!(app.find_open);
        key(&mut app, vk::ESCAPE);
        assert!(!app.find_open && app.full);
        key(&mut app, vk::ESCAPE);
        assert!(!app.full);
    }

    #[test]
    fn the_presentation_shows_one_page_fitted_to_the_screen_on_black_and_turns_with_the_keys_the_mouse_and_the_wheel() {
        // Two portrait pages, a landscape one, a portrait one.
        let mut app = open("present", &[(612, 792, 0), (612, 792, 0), (792, 612, 0), (612, 792, 0)], false);
        let actions = key(&mut app, vk::F5);
        assert!(app.present.is_some() && app.full);
        assert!(actions.iter().any(|a| matches!(a, Action::FullScreen(true))));
        assert!(actions.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_CURSOR, ms: 2000 })));
        app.event(Event::Size { w: 1920, h: 1080 });
        let shown = |app: &App| {
            let l = &app.doc.as_ref().expect("a document").layout;
            (app.visible(), l.top(app.current_page()) - app.scroll_y, l.size_px(app.current_page()))
        };
        // Page 1 alone, at the top, as high as the screen (a portrait page is limited by the height).
        let (visible, top, (pw, ph)) = shown(&app);
        assert_eq!((visible, top), (Some((0, 0)), 0));
        assert!((ph - 1080).abs() <= 1 && pw < 1920, "{pw}x{ph}");
        // Black ground, no edge.
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert_eq!(rec.fills.first().map(|f| f.1), Some(0));
        assert!(rec.fills.iter().all(|f| f.1 != app.pal.border));
        // Space, PgDn, the arrow right and down, Enter: on. Backspace, PgUp, the arrow left and up: back. Shift+Space: back.
        for vk in [vk::SPACE, vk::NEXT, vk::RIGHT] {
            key(&mut app, vk);
        }
        assert_eq!(app.current_page(), 3, "three turns on");
        let l = &app.doc.as_ref().expect("a document").layout;
        assert_eq!(app.scroll_y, l.top(3));
        for _ in 0..2 {
            key(&mut app, vk::LEFT);
        }
        // The landscape page has its own fit (the width limits it), and it is alone as well.
        let (visible, top, (pw, ph)) = shown(&app);
        assert_eq!((app.current_page(), visible, top), (1, Some((1, 1)), 0));
        key(&mut app, vk::DOWN);
        let (visible, top, (pw2, ph2)) = shown(&app);
        assert_eq!((app.current_page(), visible, top), (2, Some((2, 2)), 0));
        assert!(pw2 <= 1920 && ph2 <= 1080 && (pw2 == 1920 || ph2 == 1080) && (pw2, ph2) != (pw, ph), "{pw2}x{ph2}");
        // The last page is the end: more turns stay on it.
        key(&mut app, vk::RETURN);
        key(&mut app, vk::SPACE);
        assert_eq!(app.current_page(), 3);
        for vk in [vk::BACK, vk::PRIOR, vk::LEFT] {
            key(&mut app, vk);
        }
        assert_eq!(app.current_page(), 0, "three turns back");
        app.event(Event::Key { vk: vk::SPACE, ctrl: false, shift: true });
        assert_eq!(app.current_page(), 0);
        key(&mut app, vk::END);
        assert_eq!(app.current_page(), 3);
        key(&mut app, vk::HOME);
        assert_eq!(app.current_page(), 0);
        // A click is the next page, the right button the one before, a click of the wheel one page (up is back).
        app.event(Event::MouseDown { x: 900, y: 500, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 900, y: 500 });
        assert_eq!(app.current_page(), 1);
        app.event(Event::ContextMenu { x: 900, y: 500 });
        assert_eq!(app.current_page(), 0);
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 900, y: 500 });
        assert_eq!(app.current_page(), 1);
        app.event(Event::Wheel { delta: 120, ctrl: false, shift: false, x: 900, y: 500 });
        assert_eq!(app.current_page(), 0);
        // Esc ends it at the page shown: the window gives its place back, the zoom is the person's again and the page is at the top.
        key(&mut app, vk::SPACE);
        let end = key(&mut app, vk::ESCAPE);
        assert!(app.present.is_none() && !app.full);
        assert!(end.iter().any(|a| matches!(a, Action::FullScreen(false))) && end.iter().any(|a| matches!(a, Action::KillTimer(TIMER_CURSOR))));
        app.event(Event::Size { w: 1600, h: 900 });
        assert_eq!((app.current_page(), app.mode), (1, ZoomMode::FitWidth));
        let l = &app.doc.as_ref().expect("a document").layout;
        assert!((l.top(1) - app.scroll_y).abs() <= app.px(MARGIN), "page 2 is at the top");
        assert!(app.visible().is_some_and(|(a, _)| a == 1));
    }

    #[test]
    fn a_page_that_is_not_as_high_as_the_screen_is_in_the_middle_of_it_the_last_one_too() {
        let mut app = open("wide", &[(612, 792, 0), (1000, 300, 0)], false);
        key(&mut app, vk::F5);
        app.event(Event::Size { w: 1920, h: 1080 });
        key(&mut app, vk::SPACE);
        assert_eq!(app.current_page(), 1);
        let l = &app.doc.as_ref().expect("a document").layout;
        let (pw, ph) = l.size_px(1);
        assert!(pw >= 1919 && ph < 700, "{pw}x{ph}: the width limits it");
        assert_eq!(l.top(1) - app.scroll_y, (1080 - ph) / 2);
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert_eq!(rec.fills.first().map(|f| f.1), Some(0));
    }

    #[test]
    fn the_next_page_of_the_presentation_is_drawn_before_it_is_turned_to() {
        let mut app = open("ahead", &[(612, 792, 0); 6], false);
        key(&mut app, vk::F5);
        app.event(Event::Size { w: 1920, h: 1080 });
        pump(&mut app, "the page and the ones round it", |a| a.visible_ready() && a.is_idle());
        let start = Instant::now();
        key(&mut app, vk::SPACE);
        // Nothing is asked for and nothing is waited for: the picture of the page is in the cache when the turn is made.
        assert!(app.visible_ready(), "the next page was not ready");
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(start.elapsed().as_millis() < 50, "{:?}", start.elapsed());
        assert!(!rec.pictures.is_empty());
    }

    #[test]
    fn the_pointer_is_hidden_after_two_seconds_without_moving_and_shown_when_it_moves() {
        let mut app = open("pointer", &[(612, 792, 0); 3], false);
        key(&mut app, vk::F5);
        app.event(Event::Size { w: 1600, h: 900 });
        let moved = app.event(Event::Hover { x: 700, y: 400 });
        assert!(moved.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_CURSOR, .. })));
        assert_ne!(app.cursor_at(700, 400), Cursor::Hidden);
        let idle = app.event(Event::Timer(TIMER_CURSOR));
        assert!(idle.iter().any(|a| matches!(a, Action::HideCursor)) && idle.iter().any(|a| matches!(a, Action::KillTimer(TIMER_CURSOR))));
        assert_eq!(app.cursor_at(700, 400), Cursor::Hidden);
        // Moving shows it, and it is hidden again after the next two seconds.
        assert_ne!(app.cursor_at(701, 400), Cursor::Hidden);
        app.event(Event::Hover { x: 702, y: 400 });
        assert_ne!(app.cursor_at(702, 400), Cursor::Hidden);
        // Out of the presentation it is never hidden.
        key(&mut app, vk::ESCAPE);
        assert_ne!(app.cursor_at(702, 400), Cursor::Hidden);
        app.event(Event::Timer(TIMER_CURSOR));
        assert_ne!(app.cursor_at(702, 400), Cursor::Hidden);
    }

    #[test]
    fn the_next_page_is_ready_even_when_the_pages_are_not_all_of_one_size() {
        // The pages of a scan are never quite alike: each is fitted to the screen by itself, and the one after is drawn at its own fit already.
        let mut app = open("ahead2", &[(612, 792, 0), (600, 790, 0), (612, 780, 0), (590, 792, 0), (612, 785, 0)], false);
        key(&mut app, vk::F5);
        app.event(Event::Size { w: 1920, h: 1080 });
        for turn in 1..5 {
            pump(&mut app, "the page and the ones round it", |a| a.visible_ready() && a.is_idle());
            key(&mut app, vk::SPACE);
            assert!(app.visible_ready(), "page {} was not ready when it was turned to", turn + 1);
        }
    }

    #[test]
    fn at_the_last_page_of_the_presentation_the_first_is_not_drawn_ahead_and_a_late_timer_does_not_hide_the_pointer_of_the_window() {
        let mut app = open("last", &[(612, 792, 0); 6], false);
        key(&mut app, vk::F5);
        app.event(Event::Size { w: 1920, h: 1080 });
        key(&mut app, vk::END);
        pump(&mut app, "the last page and the one before", |a| a.visible_ready() && a.is_idle());
        assert_eq!(app.current_page(), 5);
        // What is asked for now: the last page and the one before it, whole; of the first page only the small picture of the pages round about, if that.
        let scene = app.scene().expect("a scene");
        let wanted = sched::wanted(&scene, &app.cache);
        assert!(wanted.iter().all(|w| w.preview || w.key.page >= 4), "the first page is drawn ahead at the end: {:?}", wanted.iter().filter(|w| !w.preview).map(|w| w.key.page).collect::<Vec<_>>());
        // The place that is kept in the presentation is the page shown, not the one above it (the view is above the page's top when it is not as high as the screen).
        assert_eq!(app.current_place().map(|p| p.page), Some(5));
        // The timer of the pointer that was queued when Esc came: nothing is hidden in the window it came back to.
        key(&mut app, vk::ESCAPE);
        let late = app.event(Event::Timer(TIMER_CURSOR));
        assert!(!late.iter().any(|a| matches!(a, Action::HideCursor)), "{late:?}");
        app.event(Event::Size { w: 1600, h: 900 });
        assert_ne!(app.cursor_at(0, 0), Cursor::Hidden);
    }

    #[test]
    fn the_place_kept_in_a_presentation_of_a_page_not_as_high_as_the_screen_is_that_page() {
        let mut app = open("place", &[(612, 792, 0), (1000, 300, 0), (612, 792, 0)], false);
        key(&mut app, vk::F5);
        app.event(Event::Size { w: 1920, h: 1080 });
        key(&mut app, vk::SPACE);
        assert_eq!(app.current_page(), 1);
        // The view is some 250 pixels above the top of the wide page: `locate` of it is the page before.
        let l = &app.doc.as_ref().expect("a document").layout;
        assert!(app.scroll_y < l.top(1) - 100);
        let place = app.current_place().expect("a place");
        assert_eq!((place.page, place.down, place.mode), (1, 0, crate::recent::MODE_FIT_WIDTH));
    }
}
