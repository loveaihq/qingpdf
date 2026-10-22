//! What the reader draws over the pages and over its own chrome (2026-10-21): the thin scroll bars, the page pill, the little messages (toasts), the
//! tooltips, and what ties them together, the hover and press state of every button and the clock that fades things in and out. The arithmetic of each is
//! in its own plain module (`overbar`, `pill`, `toast`); this is the part that knows the reader's state and the painter.
//!
//! The reader says where things are in the coordinates of the pages' area (the window moves them below the title strip and the command bar), but it draws on
//! the whole window: the code here that draws turns the pages' area's coordinates into the window's with [`Shifted`].

use super::tabbar::Shifted;
use super::*;
use crate::accent;
use crate::overbar;
use crate::pill::{self, Part};
use crate::tabs;
use crate::toast;
use crate::ui::Font;

/// The time between two pictures of something fading or growing, in milliseconds, and how long the pointer rests on a button before its tooltip comes.
pub(super) const ANIM_MS: u32 = 16;
pub(super) const TIP_MS: u32 = 450;

/// What the pointer is on (or is pressing): a part of the title strip, of the command bar, of the page pill, of the welcome page, or a scroll bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hot {
    Tab(usize),
    Close(usize),
    Plus,
    Cmd(usize),
    Search,
    More,
    Pill(Part),
    Open,
    Row(usize),
    BarV,
    BarH,
}

/// Which box over the pill is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PillField {
    Page,
    Zoom,
}

/// What the clock draws again at a tick: the rooms of the things that change (in the window's coordinates), or all of the window (when something moves the pages
/// or the strip).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Dirty {
    Parts(Vec<Rect>),
    Full,
}

impl Default for Dirty {
    fn default() -> Dirty {
        Dirty::Parts(Vec::new())
    }
}

/// A scroll bar being dragged: which, and how far down its thumb (in pixels) the pointer took hold.
#[derive(Clone, Copy, Debug)]
pub(super) struct BarDrag {
    vertical: bool,
    grab: i64,
}

pub(super) fn rc(x: i64, y: i64, w: i64, h: i64) -> Rect {
    Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w), h: clamp32(h) }
}

/// The mark: the character 轻 in white on a rounded teal square, in the rectangle `r` (square). At 16, 24 and 32 pixels it is the bitmap made by hand (whole pixels);
/// at any other size, the strokes.
pub(super) fn draw_logo(p: &mut dyn Painter, r: Rect) {
    let k = r.w as f32 / 100.0;
    let radius = (crate::logo::RADIUS * k).round() as i32;
    p.fill_shape(r, [radius; 4], accent::QING_LIGHT.base, 255);
    if let Some(bitmap) = crate::logo::bitmap(r.w).filter(|_| r.w == r.h) {
        for (y, row) in bitmap.iter().enumerate() {
            // A run of pixels in a row is one rectangle.
            let mut x = 0;
            while x < row.len() {
                let run = row.bytes().skip(x).take_while(|&c| c == b'#').count();
                if run > 0 {
                    p.fill_rect(Rect { x: r.x + x as i32, y: r.y + y as i32, w: run as i32, h: 1 }, 0xFFFFFF);
                }
                x += run.max(1);
            }
        }
        return;
    }
    for s in crate::logo::strokes(r.w) {
        let points: Vec<(f32, f32)> = s.pts.iter().map(|&(x, y)| (r.x as f32 + x * k, r.y as f32 + y * k)).collect();
        p.polyline(&points, s.width * k, 0xFFFFFF, 255);
    }
}

impl App {
    /// The things over the pages (bars, pill, messages, tooltips) are there: not in the speed test, nor in a window that is bare.
    pub(super) fn overlays_on(&self) -> bool {
        self.bench.is_none() && !self.bare
    }

    /// What the box over the page number of the pill is given when Enter is pressed in it (for the tests).
    #[cfg(test)]
    pub(super) fn type_page(&mut self, text: &str) -> Vec<Action> {
        self.pill_edit = Some(PillField::Page);
        self.event(Event::PillEdit(Some(text.to_string())))
    }

    /// The reader's clock, in milliseconds since it started.
    pub(super) fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Is `hot` pressed (the mouse is down on it and the pointer is still on it)?
    pub(super) fn is_pressed(&self, hot: Hot) -> bool {
        self.pressed == Some(hot) && self.hot == Some(hot)
    }

    /// The fill under a button the pointer is on (or presses): the ink at a low opacity.
    pub(super) fn hover_fill(&self, p: &mut dyn Painter, r: Rect, radius: i32, pressed: bool, scale: u8) {
        let a = if pressed { self.pal.press_alpha } else { self.pal.hover_alpha };
        p.fill_shape(r, [radius; 4], self.pal.status_fg, (u32::from(a) * u32::from(scale) / 255) as u8);
    }

    // --- the clock ----------------------------------------------------------------------------------------------

    /// Does the picture of anything change as time passes at `now` (something fades, grows or slides), so that the clock must run at its fast pace? What only
    /// stands (a message that is held, a bar that waits to begin to fade) does not count: [`App::next_wake`] says when that changes.
    fn animating(&self, now: u64) -> bool {
        self.toasts.moving(now) || self.vis_v.changing(now) || self.vis_h.changing(now) || pill::changing(now, self.pill_scrolled) || self.motion_busy(now)
    }

    /// How long (milliseconds) the clock may sleep when nothing changes now: until the message that is held ends, or the bar that stands begins to fade.
    fn next_wake(&self, now: u64) -> Option<u64> {
        [self.toasts.next_change(now), self.vis_v.hold_left(now), self.vis_h.hold_left(now), pill::hold_left(now, self.pill_scrolled)].into_iter().flatten().min()
    }

    /// The timer for the pictures of something that fades is set (once, at its fast pace).
    pub(super) fn start_anim(&mut self) -> Vec<Action> {
        if std::mem::replace(&mut self.anim_on, true) { Vec::new() } else { vec![Action::SetTimer { id: TIMER_ANIM, ms: ANIM_MS }] }
    }

    /// The timer: the next picture of what changes (only the room of it is drawn again), then the end of the timer when nothing moves any more, or a slow one
    /// that wakes the clock when something that stands is to change.
    pub(super) fn anim_tick(&mut self) -> Vec<Action> {
        let now = self.now_ms();
        self.toasts.advance(now);
        let mut actions = self.motion_tick(now);
        actions.extend(self.clock_repaint(now));
        if self.animating(now) {
            // (A tick of the slow clock that finds something changing puts it back to its fast pace.)
            if !std::mem::replace(&mut self.anim_on, true) {
                actions.push(Action::SetTimer { id: TIMER_ANIM, ms: ANIM_MS });
            }
        } else {
            self.anim_on = false;
            actions.push(match self.next_wake(now) {
                Some(ms) => Action::SetTimer { id: TIMER_ANIM, ms: u32::try_from(ms.clamp(10, 60_000)).unwrap_or(1000) + 2 },
                None => Action::KillTimer(TIMER_ANIM),
            });
        }
        actions
    }

    /// What a tick draws again: the room of each thing that changes now, and of each that changed at the tick before (so that the last picture of a fade is
    /// drawn); all of the window when something moves the pages or the strip.
    fn clock_repaint(&mut self, now: u64) -> Vec<Action> {
        let cur = self.changing_parts(now);
        match (cur.clone(), std::mem::replace(&mut self.anim_prev, cur)) {
            (Dirty::Parts(mut parts), Dirty::Parts(before)) => {
                for r in before {
                    if !parts.contains(&r) {
                        parts.push(r);
                    }
                }
                parts.into_iter().map(Action::InvalidateRect).collect()
            }
            _ => vec![Action::Invalidate],
        }
    }

    /// The rooms (in the window's coordinates) of what changes as time passes at `now`.
    fn changing_parts(&self, now: u64) -> Dirty {
        if self.fx.side.moving(now) || self.fx.slides.moving(now) || self.fx.ghost > 0 {
            return Dirty::Full;
        }
        let (grow, top) = (clamp32(self.px(2)), clamp32(self.bar_h()));
        let mut parts = Vec::new();
        for key in self.fx.hover.active(now) {
            match self.hot_rect(key) {
                Some(r) => parts.push(Rect { x: r.x - grow, y: r.y - grow, w: r.w + 2 * grow, h: r.h + 2 * grow }),
                None => return Dirty::Full,
            }
        }
        if self.toasts.moving(now) {
            parts.push(self.toast_zone());
        }
        for (vertical, vis) in [(true, &self.vis_v), (false, &self.vis_h)] {
            if vis.changing(now) && self.bar_geometry(vertical).is_some() {
                let z = self.bar_zone(vertical);
                parts.push(Rect { y: z.y + top, ..z });
            }
        }
        if pill::changing(now, self.pill_scrolled) && self.pill_visible() {
            let card = self.pill_layout().card;
            let m = clamp32(self.px(6));
            parts.push(Rect { x: card.x - m, y: card.y + top - m, w: card.w + 2 * m, h: card.h + 2 * m });
        }
        Dirty::Parts(parts)
    }

    // --- the little messages -------------------------------------------------------------------------------------

    /// The room the messages can take, in the window's coordinates: as wide as a message may be, as high as two lines, above the pill; with the room for the shadow
    /// and for the rise of a message that fades in. The part of the window that is drawn again when a message comes, changes, fades or goes.
    fn toast_zone(&self) -> Rect {
        let m = self.px(4);
        let (lh, lift) = (i64::from(self.line_h.max(16)), self.px(i64::from(toast::LIFT)));
        let (w, card_h) = ((self.view_w * 7 / 10).max(self.px(120)), 2 * lh + self.px(2) + 2 * self.px(10));
        let bottom = if self.pill_visible() { i64::from(self.pill_layout().card.y) - self.px(12) } else { self.view_h - self.px(24) };
        rc(self.view_x + (self.view_w - w) / 2 - m, bottom - card_h - m + self.bar_h(), w + 2 * m, card_h + lift + 2 * m)
    }

    /// Draw the room of the messages again.
    pub(super) fn toast_invalid(&self) -> Action {
        Action::InvalidateRect(self.toast_zone())
    }

    /// Say something over the pages for a few seconds.
    pub(super) fn say(&mut self, text: String) -> Vec<Action> {
        let now = self.now_ms();
        self.note = Some(text.clone());
        self.toasts.push(text, now);
        let mut actions = self.start_anim();
        actions.push(self.toast_invalid());
        actions
    }

    /// Esc closes the ordinary message that shows (one that stays is for its job or its tool to end). `None` when none does.
    pub(super) fn dismiss_toast(&mut self) -> Option<Vec<Action>> {
        let now = self.now_ms();
        if !self.toasts.dismiss(now) {
            return None;
        }
        let mut actions = self.start_anim();
        actions.push(self.toast_invalid());
        Some(actions)
    }

    /// A click on the message that shows closes it, a message that stays too (it is not shown again until it says something else).
    fn close_toast(&mut self) -> Vec<Action> {
        let now = self.now_ms();
        if !self.toasts.close(now) {
            return Vec::new();
        }
        let mut actions = self.start_anim();
        actions.push(self.toast_invalid());
        actions
    }

    /// Where the card of the message that shows is, in the pages' area: in the middle, above the pill. The message is `lines` lines of `line_h`, the widest `text_w`.
    fn toast_card(&self, text_w: i64, lines: usize, line_h: i64) -> Rect {
        let n = lines.max(1) as i64;
        let (w, h) = (text_w + 2 * self.px(16), n * line_h + (n - 1) * self.px(2) + 2 * self.px(10));
        let bottom = if self.pill_visible() { i64::from(self.pill_layout().card.y) - self.px(12) } else { self.view_h - self.px(24) };
        rc(self.view_x + (self.view_w - w) / 2, bottom - h, w, h)
    }

    /// The lines a message is laid in: at most two, as wide as the card may be (a long message is broken after a word, and only if it is still too long the
    /// middle of the second line is left out, so that the end of it stays: "Esc ends", "Ctrl+Enter").
    fn toast_lines(&self, p: &mut dyn Painter, text: &str) -> Vec<String> {
        p.set_font(Font::Body);
        let most = (self.view_w * 7 / 10).max(self.px(120)) - 2 * self.px(16);
        toast::wrap(text, clamp32(most.max(0)), 2, &mut |s| p.text_width(s))
    }

    fn draw_toast(&self, p: &mut dyn Painter, now: u64, lines: &[String]) {
        let Some(view) = self.toasts.view(now) else { return };
        if lines.is_empty() {
            return;
        }
        p.set_font(Font::Body);
        let (tw, lh) = (lines.iter().map(|l| i64::from(p.text_width(l))).max().unwrap_or(0), i64::from(p.line_height()));
        let mut card = self.toast_card(tw, lines.len(), lh);
        card.y += clamp32(self.px(i64::from(view.lift)));
        let a = u32::from(view.alpha);
        p.fill_shape(Rect { x: card.x - 1, y: card.y - 1, w: card.w + 2, h: card.h + 2 }, [clamp32(self.px(11)); 4], 0x000000, (u32::from(self.pal.shadow_alpha) * a / 255) as u8);
        p.fill_shape(card, [clamp32(self.px(10)); 4], self.pal.toast_bg, view.alpha);
        let ink = accent::mix(self.pal.toast_bg, self.pal.toast_fg, a + (a >> 7));
        for (i, line) in lines.iter().enumerate() {
            p.text(card.x + clamp32(self.px(16)), card.y + clamp32(self.px(10) + i as i64 * (lh + self.px(2))), line, ink);
        }
    }

    // --- the page pill -------------------------------------------------------------------------------------------

    /// The pill is there while a file is shown (not in the presentation).
    pub(super) fn pill_visible(&self) -> bool {
        self.doc.is_some() && self.present.is_none() && self.overlays_on() && self.view_w > 0
    }

    pub(super) fn pill_layout(&self) -> pill::Layout {
        let digit = i64::from(self.digit_w.max(1));
        let page_w = digit * pill::widest_page_label(self.page_count()).chars().count() as i64;
        pill::layout(rc(self.view_x, 0, self.view_w, self.view_h), self.dpi, clamp32(page_w), clamp32(digit * 5))
    }

    fn pill_alpha(&self, now: u64) -> u8 {
        let over = matches!(self.hot, Some(Hot::Pill(_))) || self.pill_edit.is_some();
        pill::alpha(now, self.pill_scrolled, over)
    }

    /// Whether the pill takes a click at this place of the pages' area: it does when it is there and not faint.
    fn pill_takes(&self, x: i64, y: i64) -> bool {
        self.pill_visible() && pill::usable(self.pill_alpha(self.now_ms())) && pill::on_card(&self.pill_layout(), clamp32(x), clamp32(y))
    }

    /// Is the button of this part of the pill usable now?
    pub(super) fn pill_part_enabled(&self, part: Part) -> bool {
        match part {
            Part::Prev => self.current_page() > 0,
            Part::Next => self.last_of_row() < self.page_count(),
            _ => true,
        }
    }

    /// A button of the pill was pressed and let go on itself.
    pub(super) fn pill_click(&mut self, part: Part) -> Vec<Action> {
        if !self.pill_part_enabled(part) {
            return Vec::new();
        }
        match part {
            Part::Prev => self.run_command(CMD_PREV_PAGE),
            Part::Next => self.run_command(CMD_NEXT_PAGE),
            Part::ZoomOut => self.run_command(CMD_ZOOM_OUT),
            Part::ZoomIn => self.run_command(CMD_ZOOM_IN),
            Part::Fit => self.run_command(if self.mode == ZoomMode::FitWidth { CMD_FIT_PAGE } else { CMD_FIT_WIDTH }),
            Part::Page | Part::Zoom => {
                let Some((_, r)) = self.pill_layout().parts.iter().find(|(p, _)| *p == part).copied() else { return Vec::new() };
                // The box is as high as a line of text, in the middle of the part.
                let eh = clamp32(i64::from(self.line_h) + self.px(4)).min(r.h);
                let r = Rect { y: r.y + (r.h - eh) / 2, h: eh, ..r };
                let (field, text) = if part == Part::Page { (PillField::Page, (self.current_page() + 1).to_string()) } else { (PillField::Zoom, ((f64::from(self.zoom) / 10.0).round() as u32).to_string()) };
                self.pill_edit = Some(field);
                vec![Action::PillEdit(Some((r, text))), Action::Invalidate]
            }
        }
    }

    /// The box over the pill is closed: with Enter (`Some`) the page or the zoom typed is taken.
    pub(super) fn pill_edited(&mut self, text: Option<String>) -> Vec<Action> {
        let Some(field) = self.pill_edit.take() else { return Vec::new() };
        let mut actions = vec![Action::Invalidate];
        let Some(text) = text else { return actions };
        match field {
            PillField::Page => {
                if let Some(page) = crate::cmdbar::parse_page_box(&text, self.page_count()) {
                    actions.extend(self.go_to_page(page));
                }
            }
            PillField::Zoom => {
                if let Some(z) = pill::parse_zoom(&text, zoom::MIN_ZOOM, zoom::MAX_ZOOM) {
                    actions.extend(self.set_zoom(ZoomMode::Custom, z, None));
                }
            }
        }
        actions
    }

    /// The glyph of a button of the pill, drawn as lines in the middle of `r` (the picture of "fit" is a letter of the icon font).
    fn pill_glyph(&self, p: &mut dyn Painter, part: Part, r: Rect, color: Rgb) {
        let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
        let k = self.dpi as f32 / 96.0;
        let (w, a) = (1.6 * k, 5.0 * k);
        match part {
            Part::Prev => p.polyline(&[(cx + 0.6 * a, cy - a), (cx - 0.4 * a, cy), (cx + 0.6 * a, cy + a)], w, color, 255),
            Part::Next => p.polyline(&[(cx - 0.6 * a, cy - a), (cx + 0.4 * a, cy), (cx - 0.6 * a, cy + a)], w, color, 255),
            Part::ZoomOut => p.polyline(&[(cx - a, cy), (cx + a, cy)], w, color, 255),
            Part::ZoomIn => {
                p.polyline(&[(cx - a, cy), (cx + a, cy)], w, color, 255);
                p.polyline(&[(cx, cy - a), (cx, cy + a)], w, color, 255);
            }
            Part::Fit => p.glyph(r, '\u{E9A6}', color),
            Part::Page | Part::Zoom => {}
        }
    }

    fn draw_pill(&self, p: &mut dyn Painter, now: u64) {
        if !self.pill_visible() {
            return;
        }
        let alpha = self.pill_alpha(now);
        let l = self.pill_layout();
        let (k, radius) = (u32::from(alpha), l.card.h / 2);
        // The shadow: three layers, each a little wider and fainter than the last.
        for (i, fraction) in [(1, 3), (2, 2), (3, 1)] {
            let grow = clamp32(self.px(i));
            let r = Rect { x: l.card.x - grow, y: l.card.y - grow + clamp32(self.px(2)), w: l.card.w + 2 * grow, h: l.card.h + 2 * grow };
            p.fill_shape(r, [radius + grow; 4], 0x000000, (u32::from(self.pal.shadow_alpha) * fraction / 4 * k / 255) as u8);
        }
        p.fill_shape(l.card, [radius; 4], self.pal.surface, alpha);
        p.stroke_round(l.card, radius, 1, self.pal.hairline, alpha);
        for s in &l.seps {
            p.fill_rect_alpha(*s, self.pal.hairline, alpha);
        }
        p.set_font(Font::Body);
        let lh = p.line_height();
        let ink = |on: bool| accent::mix(self.pal.surface, if on { self.pal.status_fg } else { self.pal.disabled_fg }, k + (k >> 7));
        for (part, r) in &l.parts {
            let enabled = self.pill_part_enabled(*part);
            let is_hot = self.hot == Some(Hot::Pill(*part)) && enabled;
            if is_hot {
                let pressed = self.is_pressed(Hot::Pill(*part));
                let rad = if matches!(part, Part::Page | Part::Zoom) { clamp32(self.px(8)) } else { r.h / 2 };
                self.hover_fill(p, *r, rad, pressed, alpha);
            }
            match part {
                Part::Page | Part::Zoom => {
                    let text = if *part == Part::Page { pill::page_label(self.current_page() + 1, self.last_of_row(), self.page_count()) } else { pill::zoom_label(self.zoom) };
                    let editing = self.pill_edit.is_some_and(|f| (f == PillField::Page) == (*part == Part::Page));
                    if !editing {
                        let w = p.text_width(&text);
                        p.text(r.x + (r.w - w) / 2, r.y + (r.h - lh) / 2, &text, ink(true));
                    }
                }
                _ => self.pill_glyph(p, *part, *r, ink(enabled)),
            }
        }
    }

    // --- the scroll bars -----------------------------------------------------------------------------------------

    /// The view moved: the bars show again, and the pill is faint for a moment.
    pub(super) fn wake_bars(&mut self) -> Vec<Action> {
        let now = self.now_ms();
        let pos = (self.scroll_x, self.scroll_y, self.doc.as_ref().map_or(0, |d| d.layout.content_height()));
        if std::mem::replace(&mut self.last_pos, pos) == pos || self.present.is_some() || !self.overlays_on() {
            return Vec::new();
        }
        self.vis_v.wake(now);
        self.vis_h.wake(now);
        self.pill_scrolled = Some(now);
        self.start_anim()
    }

    /// The bar's track in the pages' area (its start, its length, and where it stands across: the left edge and the width of the strip it is in) and the
    /// thumb (start, length along the track). `None` when there is nothing to scroll that way.
    fn bar_geometry(&self, vertical: bool) -> Option<((i64, i64), (i64, i64))> {
        if self.present.is_some() || !self.overlays_on() {
            return None;
        }
        let doc = self.doc.as_ref()?;
        let end = self.px(i64::from(overbar::END));
        let (track, content, view, pos) = if vertical {
            (self.view_h - 2 * end, doc.layout.content_height(), self.view_h, self.scroll_y)
        } else {
            (self.view_w - 2 * end, doc.layout.content_width(), self.view_w, self.scroll_x)
        };
        let thumb = overbar::thumb(track, content, view, pos, self.px(i64::from(overbar::MIN_THUMB)))?;
        Some(((end, track), thumb))
    }

    /// The strip along the right (or bottom) edge of the pages that wakes the bar and takes the mouse for it.
    fn bar_zone(&self, vertical: bool) -> Rect {
        let z = self.px(i64::from(overbar::ZONE));
        if vertical { rc(self.view_x + self.view_w - z, 0, z, self.view_h) } else { rc(self.view_x, self.view_h - z, self.view_w, z) }
    }

    /// Is (`x`, `y`) of the pages' area in the strip that wakes the bar (the pointer there makes the bar widen)?
    fn in_zone(&self, vertical: bool, x: i64, y: i64) -> bool {
        let z = self.bar_zone(vertical);
        self.bar_geometry(vertical).is_some() && x >= i64::from(z.x) && y >= i64::from(z.y) && x < i64::from(z.x) + i64::from(z.w) && y < i64::from(z.y) + i64::from(z.h)
    }

    /// Does the bar take a mouse-down at (`x`, `y`) of the pages' area? Only where it is: on its thumb while it shows, or anywhere in its strip while the pointer
    /// has been on it (it is wide then, and a click on its track pages) or it is dragged. A click in the strip of a bar that is not shown, or away from the thumb
    /// of one that is, is the page's (a link, a field, the start of a selection at the edge of the window).
    fn bar_takes(&self, vertical: bool, x: i64, y: i64, now: u64) -> bool {
        if !self.in_zone(vertical, x, y) {
            return false;
        }
        let vis = if vertical { &self.vis_v } else { &self.vis_h };
        if vis.hover || vis.drag {
            return true;
        }
        let Some(((start, _), (t0, len))) = self.bar_geometry(vertical) else { return false };
        let at = (if vertical { y } else { x - self.view_x }) - start;
        vis.alpha(now) > 0 && (t0..t0 + len).contains(&at)
    }

    /// The thumb as a rectangle in the pages' area, as wide as the bar is now.
    fn thumb_rect(&self, vertical: bool, now: u64) -> Option<Rect> {
        let ((start, _), (t0, len)) = self.bar_geometry(vertical)?;
        let vis = if vertical { &self.vis_v } else { &self.vis_h };
        let w = i64::from(vis.width(now, clamp32(self.px(i64::from(overbar::THIN))), clamp32(self.px(i64::from(overbar::WIDE)))));
        let edge = self.px(i64::from(overbar::EDGE));
        Some(if vertical { rc(self.view_x + self.view_w - edge - w, start + t0, w, len) } else { rc(self.view_x + start + t0, self.view_h - edge - w, len, w) })
    }

    fn draw_bars(&self, p: &mut dyn Painter, now: u64) {
        for vertical in [true, false] {
            let vis = if vertical { &self.vis_v } else { &self.vis_h };
            let alpha = vis.alpha(now);
            if alpha == 0 {
                continue;
            }
            let Some(r) = self.thumb_rect(vertical, now) else { continue };
            let strong = vis.hover || vis.drag;
            let a = u32::from(alpha) * if strong { 230 } else { 170 } / 255;
            p.fill_shape(r, [r.w.min(r.h) / 2; 4], self.pal.dim_fg, a as u8);
        }
    }

    /// The mouse went down at (`x`, `y`) of the pages' area in a bar's strip: the thumb is taken hold of, or the view goes a page towards the place.
    fn bar_down(&mut self, vertical: bool, x: i64, y: i64) -> Vec<Action> {
        let now = self.now_ms();
        let Some(((start, track), thumb)) = self.bar_geometry(vertical) else { return Vec::new() };
        let at = (if vertical { y } else { x - self.view_x }) - start;
        if (0..track).contains(&at) && overbar::track_click(thumb, at) == 0 {
            self.bar_drag = Some(BarDrag { vertical, grab: at - thumb.0 });
            if vertical { self.vis_v.set_drag(true, now) } else { self.vis_h.set_drag(true, now) }
            let mut actions = self.start_anim();
            actions.push(Action::Invalidate);
            return actions;
        }
        let way = i64::from(overbar::track_click(thumb, at));
        let page = if vertical { (self.view_h * 9 / 10).max(1) } else { (self.view_w * 9 / 10).max(1) };
        let mut actions = if vertical { self.scroll_by(0, way * page, false) } else { self.scroll_by(way * page, 0, false) };
        actions.extend(self.start_anim());
        actions
    }

    /// The pointer moved with a bar's thumb in hand: the view goes where the thumb says.
    fn bar_drag_to(&mut self, x: i64, y: i64) -> Vec<Action> {
        let Some(drag) = self.bar_drag else { return Vec::new() };
        let Some(((start, track), (_, len))) = self.bar_geometry(drag.vertical) else { return Vec::new() };
        let Some(doc) = self.doc.as_ref() else { return Vec::new() };
        let at = (if drag.vertical { y } else { x - self.view_x }) - start - drag.grab;
        if drag.vertical {
            let pos = overbar::pos_at(track, doc.layout.content_height(), self.view_h, len, at);
            self.scroll_to(self.scroll_x, pos, true)
        } else {
            let pos = overbar::pos_at(track, doc.layout.content_width(), self.view_w, len, at);
            self.scroll_to(pos, self.scroll_y, true)
        }
    }

    /// The mouse came up after a thumb was dragged: the pages are drawn sharp where they stopped.
    fn bar_drag_end(&mut self) -> Vec<Action> {
        let Some(drag) = self.bar_drag.take() else { return Vec::new() };
        let now = self.now_ms();
        if drag.vertical { self.vis_v.set_drag(false, now) } else { self.vis_h.set_drag(false, now) }
        let mut actions = self.after_view_change(false);
        actions.extend(self.start_anim());
        actions
    }

    // --- what the pointer is on ------------------------------------------------------------------------------------

    /// What is at this place of the window (the whole client area).
    pub(super) fn hot_at(&self, x: i64, y: i64) -> Option<Hot> {
        let (title, top) = (self.title_h(), self.bar_h());
        if y < title {
            return match tabs::hit(&self.strip(), x, y, |i| self.shows_close(i)) {
                tabs::Hit::Tab(i) => Some(Hot::Tab(i)),
                tabs::Hit::Close(i) => Some(Hot::Close(i)),
                tabs::Hit::Plus => Some(Hot::Plus),
                tabs::Hit::Nothing => None,
            };
        }
        if y < top {
            return match crate::cmdbar::hit(&self.cmd_layout(), clamp32(x), clamp32(y - title)) {
                Some(crate::cmdbar::Hit::Button(i)) => Some(Hot::Cmd(i)),
                Some(crate::cmdbar::Hit::Search) => Some(Hot::Search),
                Some(crate::cmdbar::Hit::More) => Some(Hot::More),
                None => None,
            };
        }
        let y = y - top;
        if self.welcome_shown() {
            return match crate::welcome::hit(&self.welcome_layout(), self.recent.len(), clamp32(x), clamp32(y)) {
                Some(crate::welcome::Hit::Open) => Some(Hot::Open),
                Some(crate::welcome::Hit::Row(i)) => Some(Hot::Row(i)),
                None => None,
            };
        }
        if self.pill_takes(x, y) {
            return crate::pill::hit(&self.pill_layout(), clamp32(x), clamp32(y)).map(Hot::Pill);
        }
        if self.in_zone(true, x, y) {
            return Some(Hot::BarV);
        }
        self.in_zone(false, x, y).then_some(Hot::BarH)
    }

    /// The rectangle of what is hot, in the window's client area (for the tooltip to hang under).
    fn hot_rect(&self, hot: Hot) -> Option<Rect> {
        let (title, top) = (self.title_h(), self.bar_h());
        let down = |r: Rect, dy: i64| Rect { y: r.y.saturating_add(clamp32(dy)), ..r };
        match hot {
            Hot::Tab(i) => {
                let s = self.strip();
                Some(rc(*s.xs.get(i)?, s.top, s.width, s.height))
            }
            Hot::Close(i) => {
                let (l, t, side) = self.strip().close_box(i)?;
                Some(rc(l, t, side, side))
            }
            Hot::Plus => {
                let (l, t, side) = self.strip().plus?;
                Some(rc(l, t, side, side))
            }
            Hot::Cmd(i) => self.cmd_layout().buttons.iter().find(|(n, _)| *n == i).map(|(_, r)| down(*r, title)),
            Hot::More => Some(down(self.cmd_layout().more, title)),
            Hot::Search => self.cmd_layout().search.map(|r| down(r, title)),
            Hot::Pill(part) => self.pill_layout().parts.iter().find(|(p, _)| *p == part).map(|(_, r)| down(*r, top)),
            Hot::BarV => Some(down(self.bar_zone(true), top)),
            Hot::BarH => Some(down(self.bar_zone(false), top)),
            Hot::Open | Hot::Row(_) => None,
        }
    }

    /// The tooltip of what is hot: the name and the shortcut; the full path for a tab.
    fn tip_text(&self, hot: Hot) -> Option<String> {
        let lang = self.lang;
        match hot {
            Hot::Tab(i) => self.tabs.get(i).map(|t| t.path.to_string_lossy().into_owned()),
            Hot::Close(_) => Some(crate::cmdbar::tooltip(lang, Msg::CloseTab)),
            Hot::Plus => Some(crate::cmdbar::tooltip(lang, Msg::Open)),
            Hot::Cmd(i) => crate::cmdbar::BUTTONS.get(i).map(|d| crate::cmdbar::tooltip(lang, d.tip)),
            Hot::More => Some(crate::cmdbar::tooltip(lang, Msg::MoreMenu)),
            Hot::Search => Some(crate::cmdbar::tooltip(lang, Msg::Find)),
            // (The zoom is typed in where it stands, and has no tooltip.)
            Hot::Pill(Part::Zoom) => None,
            Hot::Pill(part) => Some(crate::cmdbar::tooltip(
                lang,
                match part {
                    Part::Prev => Msg::PrevPage,
                    Part::Next => Msg::NextPage,
                    Part::ZoomOut => Msg::ZoomOut,
                    Part::ZoomIn => Msg::ZoomIn,
                    Part::Fit => if self.mode == ZoomMode::FitWidth { Msg::FitPage } else { Msg::FitWidth },
                    Part::Page | Part::Zoom => Msg::PageNumber,
                },
            )),
            Hot::Row(i) => self.recent.get(i).map(|e| e.path.clone()),
            Hot::Open | Hot::BarV | Hot::BarH => None,
        }
    }

    /// The pointer is on `hot` now: what changes with it (the look of the button, the bar's width, the tooltip's timer).
    pub(super) fn set_hot(&mut self, hot: Option<Hot>) -> Vec<Action> {
        if hot == self.hot {
            return Vec::new();
        }
        let now = self.now_ms();
        let was = std::mem::replace(&mut self.hot, hot);
        self.tip_on = false;
        let mut actions = vec![Action::Invalidate];
        actions.extend(self.hover_moved(was, hot));
        for (bar, vertical) in [(Hot::BarV, true), (Hot::BarH, false)] {
            if was == Some(bar) || hot == Some(bar) {
                let on = hot == Some(bar);
                if vertical { self.vis_v.set_hover(on, now) } else { self.vis_h.set_hover(on, now) }
                actions.extend(self.start_anim());
            }
        }
        match hot.and_then(|h| self.tip_text(h)) {
            Some(_) => actions.push(Action::SetTimer { id: TIMER_TIP, ms: TIP_MS }),
            None => actions.push(Action::KillTimer(TIMER_TIP)),
        }
        actions
    }

    /// The pointer rests: the tooltip shows.
    pub(super) fn tip_timer(&mut self) -> Vec<Action> {
        self.tip_on = self.hot.is_some_and(|h| self.tip_text(h).is_some()) && self.pressed.is_none();
        vec![Action::KillTimer(TIMER_TIP), Action::Invalidate]
    }

    fn draw_tip(&self, p: &mut dyn Painter) {
        let Some(hot) = self.hot.filter(|_| self.tip_on) else { return };
        let (Some(text), Some(r)) = (self.tip_text(hot), self.hot_rect(hot)) else { return };
        p.set_font(Font::Body);
        let (pad, lh, edge) = (clamp32(self.px(8)), p.line_height(), clamp32(self.px(4)));
        // (A path is longer than a narrow window: it is cut in the middle, so that it keeps its drive and the name of its file, to what the window has room for.)
        let text = crate::ui::fit_middle(p, &text, (clamp32(self.client_w) - 2 * (pad + edge)).max(0));
        let (w, h) = (p.text_width(&text) + 2 * pad, lh + clamp32(self.px(10)));
        let x = clamp_to(r.x + (r.w - w) / 2, edge, clamp32(self.client_w).saturating_sub(w + edge));
        let y = if i64::from(r.y) + i64::from(r.h) + self.px(8) + i64::from(h) > self.client_h { r.y - h - clamp32(self.px(6)) } else { r.y + r.h + clamp32(self.px(6)) };
        let card = Rect { x, y, w, h };
        let radius = clamp32(self.px(6));
        p.fill_shape(Rect { x: x - 1, y: y + 1, w: w + 2, h: h + 2 }, [radius + 1; 4], 0x000000, self.pal.shadow_alpha);
        p.fill_shape(card, [radius; 4], self.pal.tip_bg, 255);
        p.stroke_round(card, radius, 1, self.pal.hairline, 255);
        p.text(x + pad, y + (h - lh) / 2, &text, self.pal.tip_fg);
    }

    // --- events ------------------------------------------------------------------------------------------------------

    /// The pointer moved with no button down, at (`x`, `y`) of the window (`None`: it left the window).
    pub(super) fn hover_at(&mut self, at: Option<(i64, i64)>) -> Vec<Action> {
        let hot = at.and_then(|(x, y)| self.hot_at(x, y));
        let top = self.bar_h();
        let row = self.side_hover(at.map(|(x, y)| (x, y - top)));
        let mut actions = self.set_hot(hot);
        if row && actions.is_empty() {
            actions.push(Action::Invalidate);
        }
        actions
    }

    /// The left button went down at (`x`, `y`) of the window: `Some` when what is there is the reader's own chrome or overlay (a title strip, a button, the pill,
    /// a bar, a message, the welcome page).
    pub(super) fn chrome_down(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        let top = self.bar_h();
        let now = self.now_ms();
        self.tip_on = false;
        let mut actions = vec![Action::KillTimer(TIMER_TIP)];
        if y >= top {
            let py = y - top;
            // A message goes at a click on it.
            if self.toasts.is_showing(now)
                && let Some(c) = self.toast_rect
                && x >= i64::from(c.x)
                && py >= i64::from(c.y)
                && x < i64::from(c.x) + i64::from(c.w)
                && py < i64::from(c.y) + i64::from(c.h)
            {
                actions.extend(self.close_toast());
                return Some(actions);
            }
            match self.hot_at(x, y) {
                Some(Hot::BarV) if self.bar_takes(true, x, py, now) => {
                    actions.extend(self.bar_down(true, x, py));
                    return Some(actions);
                }
                Some(Hot::BarH) if self.bar_takes(false, x, py, now) => {
                    actions.extend(self.bar_down(false, x, py));
                    return Some(actions);
                }
                Some(hot @ (Hot::Pill(_) | Hot::Open | Hot::Row(_))) => {
                    self.hot = Some(hot);
                    self.pressed = Some(hot);
                    self.chrome_press = true;
                    actions.push(Action::Invalidate);
                    return Some(actions);
                }
                _ => {}
            }
            // The card of the pill takes a click between its buttons too (it is not the page's).
            if self.pill_takes(x, py) {
                self.chrome_press = true;
                return Some(actions);
            }
            return None;
        }
        let hot = self.hot_at(x, y);
        self.hot = hot;
        self.chrome_press = true;
        match hot {
            // A tab is shown as the button goes down (as a browser does); the others act when the button comes up on the same thing.
            Some(Hot::Tab(i)) => actions.extend(self.show_tab(i)),
            Some(hot) => {
                self.pressed = Some(hot);
                actions.push(Action::Invalidate);
            }
            None => {}
        }
        Some(actions)
    }

    /// The pointer moved with the left button down: a bar is dragged, or what is pressed is hot while the pointer is over it.
    pub(super) fn chrome_move(&mut self, x: i64, y: i64) -> Vec<Action> {
        if self.bar_drag.is_some() {
            return self.bar_drag_to(x, y - self.bar_h());
        }
        let hot = self.hot_at(x, y);
        self.set_hot(hot)
    }

    /// Another window took the mouse while it was down: nothing is done.
    pub(super) fn chrome_cancel(&mut self) -> Vec<Action> {
        self.pressed = None;
        self.chrome_press = false;
        let mut actions = self.bar_drag_end();
        actions.push(Action::Invalidate);
        actions
    }

    /// The left button came up at (`x`, `y`): a bar is let go, or the thing that was pressed is done if the pointer is still on it.
    pub(super) fn chrome_up(&mut self, x: i64, y: i64) -> Vec<Action> {
        if self.bar_drag.is_some() {
            return self.bar_drag_end();
        }
        self.chrome_press = false;
        let Some(pressed) = self.pressed.take() else { return vec![Action::Invalidate] };
        let mut actions = vec![Action::Invalidate];
        if self.hot_at(x, y) != Some(pressed) {
            return actions;
        }
        actions.extend(self.activate(pressed));
        actions
    }

    /// What a button does.
    fn activate(&mut self, hot: Hot) -> Vec<Action> {
        match hot {
            Hot::Cmd(i) => self.cmd_activate(i),
            Hot::More => self.open_menu(false),
            Hot::Pill(part) => self.pill_click(part),
            Hot::Open => self.run_command(CMD_OPEN),
            Hot::Row(i) => self.open_recent(i),
            Hot::Plus => self.run_command(CMD_OPEN),
            Hot::Close(i) => self.close_tab(i),
            Hot::Search => self.open_find(),
            Hot::Tab(_) | Hot::BarV | Hot::BarH => Vec::new(),
        }
    }

    /// The menus as a pop-up under the "⋯" button (from the keyboard, with the first item chosen). Without the command bar they come at the top right.
    pub(super) fn open_menu(&mut self, keyboard: bool) -> Vec<Action> {
        let (x, y) = if self.toolbar_visible() {
            let l = self.cmd_layout();
            (l.more.x + l.more.w, clamp32(self.title_h()) + l.more.y + l.more.h)
        } else {
            (clamp32(self.client_w) - clamp32(self.px(12)), clamp32(self.title_h()))
        };
        // (In the coordinates of the pages' area: the window moves it down by what is above them.)
        vec![Action::ShowMenu { at: (x, y - clamp32(self.bar_h())), keyboard }]
    }

    // --- drawing ---------------------------------------------------------------------------------------------------

    /// Draw what lies over the pages (the bars, the pill, a message), in the window's own coordinates.
    pub(super) fn draw_overlays(&mut self, p: &mut dyn Painter) {
        if !self.overlays_on() {
            return;
        }
        let now = self.now_ms();
        let dy = clamp32(self.bar_h());
        let mut shifted = Shifted { inner: p, dx: 0, dy };
        let p = &mut shifted;
        p.set_font(Font::Body);
        let lines = match self.toasts.view(now) {
            Some(v) if self.present.is_none() => self.toast_lines(p, v.text),
            _ => Vec::new(),
        };
        let (tw, lh) = (lines.iter().map(|l| i64::from(p.text_width(l))).max().unwrap_or(0), i64::from(p.line_height()));
        // (Where the card is, for a click on it.)
        self.toast_rect = (!lines.is_empty()).then(|| self.toast_card(tw, lines.len(), lh));
        if !self.welcome_shown() {
            self.draw_bars(p, now);
            self.draw_pill(p, now);
        }
        self.draw_toast(p, now, &lines);
    }

    /// Draw the tooltip last, over everything (it hangs from a button that may be in the title strip).
    pub(super) fn draw_tooltip(&self, p: &mut dyn Painter) {
        if self.overlays_on() {
            self.draw_tip(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::{Rec, open_pages};
    use super::*;
    use crate::overbar::Vis;

    fn app_with_pages(name: &str, pages: usize) -> App {
        let mut app = open_pages(name, &vec![(612, 792, 0); pages], false);
        app.bare = false;
        app.event(Event::Size { w: 1400, h: 900 });
        app.event(Event::Command(CMD_FIT_WIDTH));
        app
    }

    fn client(app: &App, page_y: i64) -> i64 {
        page_y + app.bar_h()
    }

    #[test]
    fn a_message_is_said_over_the_pages_one_at_a_time_and_esc_closes_it() {
        let mut app = app_with_pages("toast", 3);
        app.say("first".to_string());
        app.say("second".to_string());
        assert!(app.toasts.is_showing(app.now_ms()));
        // It is drawn: its words are in what the painter was told.
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.texts.iter().any(|(_, _, s)| s == "first") && !rec.texts.iter().any(|(_, _, s)| s == "second"));
        // Esc closes the one that shows (and does nothing else: no selection, no find box was open).
        let actions = app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
        assert!(actions.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_ANIM, .. })) || app.anim_on);
        assert!(!app.toasts.is_showing(app.now_ms()));
    }

    #[test]
    fn the_pill_turns_pages_zooms_and_fits_with_a_click_on_its_buttons() {
        let mut app = app_with_pages("pill", 5);
        let l = app.pill_layout();
        let click = |app: &mut App, part: Part| {
            // (The pill is faint right after a scroll and lets the click through: it is back, as it is a moment after.)
            app.pill_scrolled = None;
            let r = l.parts.iter().find(|(p, _)| *p == part).unwrap().1;
            let (x, y) = (r.x + r.w / 2, client(app, i64::from(r.y + r.h / 2)) as i32);
            app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
            app.event(Event::MouseUp { x, y });
        };
        assert_eq!(app.current_page(), 0);
        click(&mut app, Part::Next);
        assert_eq!(app.current_page(), 1);
        click(&mut app, Part::Prev);
        assert_eq!(app.current_page(), 0);
        // (The pill is faint right after a scroll and lets the click through: wait for it to come back.)
        app.pill_scrolled = None;
        let before = app.zoom;
        click(&mut app, Part::ZoomIn);
        let zoomed = app.zoom;
        assert!(zoomed > before && app.mode == ZoomMode::Custom);
        click(&mut app, Part::ZoomOut);
        assert!(app.zoom < zoomed);
        // The fit button fits the width first, and then goes between that and the whole page.
        click(&mut app, Part::Fit);
        assert_eq!(app.mode, ZoomMode::FitWidth);
        click(&mut app, Part::Fit);
        assert_eq!(app.mode, ZoomMode::FitPage);
        click(&mut app, Part::Fit);
        assert_eq!(app.mode, ZoomMode::FitWidth);
    }

    #[test]
    fn the_page_and_the_zoom_are_typed_in_a_box_over_the_pill() {
        let mut app = app_with_pages("typed", 9);
        app.pill_scrolled = None;
        let r = app.pill_layout().parts.iter().find(|(p, _)| *p == Part::Page).unwrap().1;
        let (x, y) = (r.x + 3, client(&app, i64::from(r.y + 3)) as i32);
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        let actions = app.event(Event::MouseUp { x, y });
        let (rect, text) = actions.iter().find_map(|a| if let Action::PillEdit(Some(b)) = a { Some(b.clone()) } else { None }).expect("the box opens");
        assert_eq!(text, "1");
        // In the window's place, under the title strip and the command bar: as high as a line of text, in the middle of the part.
        assert_eq!(rect.y + rect.h / 2, r.y + r.h / 2 + app.bar_h() as i32);
        assert!(rect.h < r.h && rect.h >= app.line_h && (rect.x, rect.w) == (r.x, r.w));
        // Enter with a page in it goes there; Esc (none) does nothing.
        app.event(Event::PillEdit(Some("7".to_string())));
        assert_eq!(app.current_page(), 6);
        app.pill_edit = Some(PillField::Page);
        app.event(Event::PillEdit(None));
        assert_eq!(app.current_page(), 6);
        // The zoom: "150" is 150 percent; what is not a number changes nothing.
        let z = app.pill_layout().parts.iter().find(|(p, _)| *p == Part::Zoom).unwrap().1;
        let (x, y) = (z.x + 3, client(&app, i64::from(z.y + 3)) as i32);
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        let actions = app.event(Event::MouseUp { x, y });
        assert!(actions.iter().any(|a| matches!(a, Action::PillEdit(Some(_)))));
        app.event(Event::PillEdit(Some("150%".to_string())));
        assert_eq!((app.zoom, app.mode), (1500, ZoomMode::Custom));
        app.pill_edit = Some(PillField::Zoom);
        app.event(Event::PillEdit(Some("lots".to_string())));
        assert_eq!(app.zoom, 1500);
    }

    #[test]
    fn the_scroll_bar_shows_after_a_scroll_is_dragged_and_a_click_on_its_track_goes_a_page() {
        let mut app = app_with_pages("bar", 12);
        // Nothing has scrolled yet: the bar is not there.
        (app.vis_v, app.vis_h, app.pill_scrolled) = (Vis::default(), Vis::default(), None);
        let now = app.now_ms();
        assert_eq!(app.vis_v.alpha(now), 0);
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 400, y: client(&app, 300) as i32 });
        assert!(app.scroll_y > 0);
        assert_eq!(app.vis_v.alpha(app.now_ms()), 255, "a scroll wakes the bar");
        // It is drawn: a thin shape at the right edge.
        let mut rec = Rec::default();
        app.paint(&mut rec);
        // Hover on its strip widens it and keeps it; dragging the thumb to the bottom scrolls to the end.
        let (x, y) = (1400 - 5, client(&app, 300) as i32);
        app.event(Event::Hover { x, y });
        assert_eq!(app.hot, Some(Hot::BarV));
        let thumb = app.thumb_rect(true, app.now_ms() + 1000).unwrap();
        let (gx, gy) = (thumb.x + thumb.w / 2, client(&app, i64::from(thumb.y + 5)) as i32);
        app.event(Event::MouseDown { x: gx, y: gy, ctrl: false, shift: false, clicks: 1 });
        assert!(app.bar_drag.is_some());
        let far = client(&app, app.view_h + 500) as i32;
        app.event(Event::MouseMove { x: gx, y: far, shift: false });
        app.event(Event::MouseUp { x: gx, y: far });
        assert!(app.bar_drag.is_none());
        let max = app.doc.as_ref().unwrap().layout.max_scroll(app.view_w, app.view_h).1;
        assert_eq!(app.scroll_y, max, "the end of the document");
        // A click on the track above the thumb goes a page back.
        let before = app.scroll_y;
        app.event(Event::Hover { x, y: client(&app, 40) as i32 });
        app.event(Event::MouseDown { x: gx, y: client(&app, 40) as i32, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: gx, y: client(&app, 40) as i32 });
        assert!(app.scroll_y < before, "{} {}", app.scroll_y, before);
    }

    #[test]
    fn the_bars_go_a_second_after_the_last_scroll_and_the_pill_is_faint_while_it_scrolls() {
        let mut app = app_with_pages("fade", 12);
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 400, y: client(&app, 300) as i32 });
        let now = app.now_ms();
        assert_eq!(app.vis_v.alpha(now + overbar::IDLE_MS + overbar::FADE_MS + 5), 0);
        assert_eq!(pill::alpha(now + 100, app.pill_scrolled, false), 60);
        assert_eq!(pill::alpha(now + 5000, app.pill_scrolled, false), 255);
        // The clock goes on while something fades, and stops when nothing does.
        assert!(app.anim_on);
    }

    #[test]
    fn the_pill_is_not_there_without_a_file_or_in_the_presentation() {
        let mut app = app_with_pages("hidden", 3);
        assert!(app.pill_visible());
        app.event(Event::Command(CMD_PRESENT));
        assert!(!app.pill_visible());
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(app.bar_geometry(true).is_none());
    }

    #[test]
    fn the_pointer_on_a_button_gives_its_tooltip_after_a_moment() {
        let mut app = app_with_pages("tip", 3);
        let l = app.cmd_layout();
        let r = l.buttons.iter().find(|(i, _)| crate::cmdbar::BUTTONS[*i].cmd == CMD_PRINT).unwrap().1;
        let actions = app.event(Event::Hover { x: r.x + 5, y: app.title_h() as i32 + r.y + 5 });
        assert!(actions.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_TIP, .. })));
        assert!(!app.tip_on);
        app.event(Event::Timer(TIMER_TIP));
        assert!(app.tip_on);
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.texts.iter().any(|(_, _, s)| s == "Print (Ctrl+P)"), "{:?}", rec.texts);
        // Leaving the window takes it away.
        app.event(Event::HoverEnd);
        assert!(!app.tip_on || app.hot.is_none());
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(!rec.texts.iter().any(|(_, _, s)| s == "Print (Ctrl+P)"));
    }

    /// The lines of the message on the card: what the painter was told to write that is part of `text` (the pages say nothing like it).
    fn card_lines(app: &mut App, text: &str) -> Vec<String> {
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let key = &text[..20.min(text.len())];
        let tail: String = text.chars().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
        rec.texts.into_iter().map(|(_, _, s)| s).filter(|s| s.len() > 12 && (text.contains(s.as_str()) || s.starts_with(key) || s.ends_with(&tail))).collect()
    }

    #[test]
    fn a_long_message_is_laid_in_two_lines_and_the_end_of_it_stays_even_when_it_is_cut() {
        let mut app = app_with_pages("toast2", 3);
        // (A card is at most 70 % of the pages' width, less its margins; a character is 7 pixels wide in the tests.)
        let most = usize::try_from(app.view_w * 7 / 10 - 32).unwrap();
        let hint = "Drag a box for the text, type, then click outside it or press Ctrl+Enter. Esc ends.".to_string();
        app.say(hint.clone());
        let lines = card_lines(&mut app, &hint);
        assert_eq!(lines.len(), 1, "it fits one line: {lines:?}");
        let long = format!("{hint} {hint} Esc ends it.");
        app.toasts.clear();
        app.say(long.clone());
        let lines = card_lines(&mut app, &long);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|l| 7 * l.chars().count() <= most), "{lines:?}");
        assert!(lines[1].ends_with("Esc ends it."), "the end stays: {lines:?}");
        // Too long even for two lines: the middle of the second is left out, the end is not.
        let longer = format!("{long} {long} {long} Ctrl+Enter places it, Esc ends it.");
        app.toasts.clear();
        app.say(longer.clone());
        let lines = card_lines(&mut app, &longer);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains('\u{2026}') && lines[1].ends_with("Esc ends it."), "{lines:?}");
        // Its card is as high as two lines, and a click on it closes it.
        let card = app.toast_rect.expect("the card");
        assert!(card.h > 2 * 16, "{card:?}");
        let (x, y) = (card.x + card.w / 2, card.y + card.h / 2 + app.bar_h() as i32);
        let before = app.toasts.is_showing(app.now_ms());
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        assert!(before && !app.toasts.is_showing(app.now_ms()));
    }

    #[test]
    fn a_message_that_fades_asks_for_its_own_room_only_and_the_pages_are_not_asked_to_be_drawn_again() {
        let mut app = app_with_pages("fade-rect", 3);
        // (What the opening of the file woke is let go, so that only the message moves.)
        app.vis_v = Vis::default();
        app.vis_h = Vis::default();
        app.pill_scrolled = None;
        app.toasts.clear();
        app.anim_prev = Dirty::default();
        app.say("Copied 12 characters".to_string());
        let (zone, client) = (app.toast_zone(), (app.client_w, app.client_h));
        assert!(i64::from(zone.w) * i64::from(zone.h) * 8 < client.0 * client.1, "the room of a message is a small part of the window: {zone:?}");
        let tick = app.event(Event::Timer(TIMER_ANIM));
        assert!(!tick.iter().any(|a| matches!(a, Action::Invalidate)), "{tick:?}");
        let rects: Vec<Rect> = tick.iter().filter_map(|a| if let Action::InvalidateRect(r) = a { Some(*r) } else { None }).collect();
        assert!(!rects.is_empty() && rects.iter().all(|r| *r == zone), "{rects:?} {zone:?}");
        // The middle of the pages and the strip above them are not in it.
        let (mx, my) = (i32::try_from(app.client_w / 2).unwrap(), i32::try_from(app.client_h / 2).unwrap());
        assert!(rects.iter().all(|r| !(r.x <= mx && mx < r.x + r.w && r.y <= my && my < r.y + r.h)) && rects.iter().all(|r| i64::from(r.y) > app.bar_h()));
        // The same room is asked for while the message goes (so that the last picture of it is drawn, too).
        std::thread::sleep(std::time::Duration::from_millis(toast::FADE_IN + 40));
        let held = app.event(Event::Timer(TIMER_ANIM));
        assert!(held.iter().all(|a| !matches!(a, Action::Invalidate)), "{held:?}");
    }

    #[test]
    fn a_message_that_is_held_lets_the_clock_sleep_until_it_ends_and_a_message_that_stays_lets_it_stop() {
        let mut app = app_with_pages("hold-clock", 3);
        app.vis_v = Vis::default();
        app.vis_h = Vis::default();
        app.pill_scrolled = None;
        app.toasts.clear();
        app.say("Saved.".to_string());
        std::thread::sleep(std::time::Duration::from_millis(toast::FADE_IN + 60));
        let tick = app.event(Event::Timer(TIMER_ANIM));
        // It stands in full: no fast clock, and one slow timer set for about when it will begin to go (seconds, not 16 milliseconds).
        let slow: Vec<u32> = tick.iter().filter_map(|a| if let Action::SetTimer { id: TIMER_ANIM, ms } = a { Some(*ms) } else { None }).collect();
        assert!(!app.anim_on && slow.len() == 1 && slow[0] > 3_000 && slow[0] <= 6_000, "{slow:?} {tick:?}");
        // A message that stays asks for no timer at all once it has faded in.
        let mut app = app_with_pages("stay-clock", 3);
        app.vis_v = Vis::default();
        app.vis_h = Vis::default();
        app.pill_scrolled = None;
        app.event(Event::Command(CMD_TOOL_INK));
        std::thread::sleep(std::time::Duration::from_millis(toast::FADE_IN + 60));
        let tick = app.event(Event::Timer(TIMER_ANIM));
        assert!(!app.anim_on && tick.iter().any(|a| matches!(a, Action::KillTimer(TIMER_ANIM))) && !tick.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_ANIM, .. })), "{tick:?}");
    }

    #[test]
    fn a_bar_that_fades_asks_for_the_strip_it_is_in_and_no_more() {
        let mut app = app_with_pages("bar-rect", 8);
        app.toasts.clear();
        app.pill_scrolled = None;
        app.vis_h = Vis::default();
        // The bar was woken a second ago: it begins to fade.
        let now = app.now_ms().max(1100);
        app.vis_v = Vis { woken: Some(now - 1100), hover: false, drag: false, changed: 0 };
        app.anim_prev = Dirty::default();
        let strip = app.bar_zone(true);
        let tick = app.clock_repaint(now);
        let rects: Vec<Rect> = tick.iter().filter_map(|a| if let Action::InvalidateRect(r) = a { Some(*r) } else { None }).collect();
        assert!(!tick.iter().any(|a| matches!(a, Action::Invalidate)) && !rects.is_empty(), "{tick:?}");
        assert!(rects.iter().all(|r| r.x == strip.x && r.w == strip.w), "{rects:?} {strip:?}");
    }

    #[test]
    fn a_click_in_the_strip_of_a_bar_that_is_not_shown_goes_to_the_page_and_a_click_on_the_thumb_takes_hold_of_it() {
        let mut app = app_with_pages("bar-click", 8);
        let ((start, track), (t0, len)) = app.bar_geometry(true).expect("a bar");
        let x = i32::try_from(app.view_x + app.view_w - app.px(5)).unwrap();
        let top = app.bar_h();
        let down = |app: &mut App, y: i64| app.event(Event::MouseDown { x, y: i32::try_from(y + top).unwrap(), ctrl: false, shift: false, clicks: 1 });
        // The bar is not shown (it was woken long ago): the page gets the click, wherever in the strip it is.
        app.vis_v = Vis::default();
        down(&mut app, start + t0 + len / 2);
        assert!(app.bar_drag.is_none() && !app.chrome_press, "the page has it");
        app.event(Event::MouseUp { x, y: 0 });
        // The bar is shown (just scrolled): a click on its thumb takes hold of it, a click on the track away from the thumb is the page's still (the pointer has
        // not been on the bar).
        let now = app.now_ms();
        app.vis_v.wake(now);
        let away = start + (t0 + len + 40).min(track - 2);
        assert!(away > start + t0 + len);
        down(&mut app, away);
        assert!(app.bar_drag.is_none() && !app.chrome_press);
        app.event(Event::MouseUp { x, y: 0 });
        down(&mut app, start + t0 + len / 2);
        assert!(app.bar_drag.is_some(), "the thumb is taken hold of");
        app.event(Event::MouseUp { x, y: 0 });
        // Once the pointer has been on the bar (it is wide) a click on its track pages, as a bar does.
        app.event(Event::Hover { x, y: i32::try_from(away + top).unwrap() });
        assert!(app.vis_v.hover);
        let before = app.scroll_y;
        down(&mut app, away);
        assert!(app.scroll_y > before, "{before} -> {}", app.scroll_y);
    }

    #[test]
    fn the_tip_of_a_tab_with_a_long_path_is_cut_to_the_window_and_never_panics_however_narrow_it_is() {
        let mut app = app_with_pages("longtip", 3);
        let long = format!("C:\\{}file.pdf", "a-very-long-name-of-a-folder\\".repeat(20));
        assert!(long.len() > 500);
        app.path = Some(std::path::PathBuf::from(&long));
        app.sync_tab();
        assert_eq!(app.tabs.get(0).map(|t| t.path.to_string_lossy().into_owned()), Some(long.clone()));
        // (The tooltip is the full path; at every width of the window, from a wide one to a sliver, it is drawn and fits the window.)
        for w in [1400i32, 700, 420, 160, 60, 12, 1] {
            app.event(Event::Size { w, h: 900 });
            app.hot = Some(Hot::Tab(0));
            app.tip_on = true;
            let mut rec = Rec::default();
            app.paint(&mut rec);
            let tip = rec.texts.iter().find(|(_, _, s)| s.starts_with("C:")).cloned();
            if w >= 160 {
                let (x, _, text) = tip.unwrap_or_else(|| panic!("the tip of a tab is drawn at {w}: {:?}", rec.texts));
                assert!(x >= 0 && x + 7 * text.chars().count() as i32 <= w, "{w}: {x} {text}");
                // The file's name is what is kept at the end.
                assert!(w < 300 || text.ends_with("file.pdf"), "{text}");
            }
        }
    }
}
