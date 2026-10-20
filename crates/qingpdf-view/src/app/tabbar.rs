//! The strip of tabs above the pages (2026-10-17): drawn here, in the window's own painter, and only when two files or more are open.
//! Each tab says the name of its file, a dot when the file has changes that are not saved, and a cross to close it; the full path of the
//! tab under the pointer is shown in a small box. What a place of the strip is on is plain arithmetic in `tabs.rs`.

use super::*;
use crate::tabs::{self, CLOSE_EDGE, CLOSE_SIDE, Hit, MAX_TAB_W, MIN_TAB_W, WIDE_TAB};
use crate::ui::Rgb;

const BAR_BG: u32 = 0xD4D4D4;
const TAB_OFF: u32 = 0xE2E2E2;
const TAB_ON: u32 = 0xF8F8F8;
const TAB_LINE: u32 = 0x2B7CFF;
const TAB_EDGE: u32 = 0xA8A8A8;
const TAB_FG: u32 = 0x303030;
const DIRTY_FG: u32 = 0xD0560B;
const TIP_BG: u32 = 0xFFFFE1;
const TIP_FG: u32 = 0x202020;
/// Space left and right of a name, and the room a dot before it takes, at 96 dpi.
const PAD: i64 = 8;
const DOT: i64 = 12;

/// The painter of the pages when the strip of tabs is above them: everything is drawn the strip's height lower.
pub(super) struct Shifted<'a> {
    pub(super) inner: &'a mut dyn Painter,
    pub(super) dy: i32,
}

impl Shifted<'_> {
    fn down(&self, r: Rect) -> Rect {
        Rect { y: r.y.saturating_add(self.dy), ..r }
    }
}

impl Painter for Shifted<'_> {
    fn fill_rect(&mut self, r: Rect, color: Rgb) {
        let r = self.down(r);
        self.inner.fill_rect(r, color);
    }

    fn fill_rect_alpha(&mut self, r: Rect, color: Rgb, alpha: u8) {
        let r = self.down(r);
        self.inner.fill_rect_alpha(r, color, alpha);
    }

    fn draw_bgra(&mut self, dest: Rect, src_w: i32, src_h: i32, bgra: &[u8]) {
        let dest = self.down(dest);
        self.inner.draw_bgra(dest, src_w, src_h, bgra);
    }

    fn text(&mut self, x: i32, y: i32, s: &str, color: Rgb) {
        let y = y.saturating_add(self.dy);
        self.inner.text(x, y, s, color);
    }

    fn text_width(&mut self, s: &str) -> i32 {
        self.inner.text_width(s)
    }

    fn line_height(&self) -> i32 {
        self.inner.line_height()
    }
}

impl App {
    /// The width of each tab, in pixels.
    pub(super) fn tab_w(&self) -> i64 {
        tabs::tab_width(self.tabs.len(), self.client_w, self.px(MIN_TAB_W), self.px(MAX_TAB_W))
    }

    /// Does tab `i` show its close button? The shown one does, the one under the pointer does, and so does any that is wide enough.
    fn shows_close(&self, i: usize, w: i64) -> bool {
        self.tabs.active() == Some(i) || self.tab_hover == Some(i) || w >= self.px(WIDE_TAB)
    }

    /// What a place of the strip is on (`x` across the window).
    fn strip_hit(&self, x: i64) -> Hit {
        let w = self.tab_w();
        tabs::hit(self.tabs.len(), w, x, self.px(CLOSE_SIDE), self.px(CLOSE_EDGE), |i| self.shows_close(i, w))
    }

    /// A mouse button went down in the strip.
    pub(super) fn strip_down(&mut self, x: i64) -> Vec<Action> {
        self.strip_press = true;
        match self.strip_hit(x) {
            Hit::Tab(i) => self.show_tab(i),
            Hit::Close(i) => self.close_tab(i),
            Hit::Nothing => Vec::new(),
        }
    }

    /// The middle button came up in the strip: it closes the tab under it.
    pub(super) fn strip_middle(&mut self, x: i64) -> Vec<Action> {
        match self.strip_hit(x) {
            Hit::Tab(i) | Hit::Close(i) => self.close_tab(i),
            Hit::Nothing => Vec::new(),
        }
    }

    /// The pointer is at `x` in the strip (`None`: it is not in the strip): the box with the path follows it.
    pub(super) fn strip_hover(&mut self, x: Option<i64>) -> Vec<Action> {
        let now = x.and_then(|x| match self.strip_hit(x) {
            Hit::Tab(i) | Hit::Close(i) => Some(i),
            Hit::Nothing => None,
        });
        if now == self.tab_hover {
            return Vec::new();
        }
        self.tab_hover = now;
        vec![Action::Invalidate]
    }

    /// Draw the strip over the top `bar_h()` rows of the window.
    pub(super) fn draw_tabbar(&self, p: &mut dyn Painter) {
        let bar = self.bar_h();
        if bar == 0 {
            return;
        }
        let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w), h: clamp32(h) };
        p.fill_rect(rect(0, 0, self.client_w, bar), BAR_BG);
        let w = self.tab_w();
        let line = self.px(1).max(1);
        let ty = clamp32((bar - i64::from(p.line_height())) / 2);
        for (i, tab) in self.tabs.iter().enumerate() {
            let x = i as i64 * w;
            let shown = self.tabs.active() == Some(i);
            p.fill_rect(rect(x, 0, w - line, bar), if shown { TAB_ON } else { TAB_OFF });
            if shown {
                p.fill_rect(rect(x, 0, w - line, self.px(2)), TAB_LINE);
            } else {
                p.fill_rect(rect(x, bar - line, w, line), TAB_EDGE);
            }
            let close = self.shows_close(i, w);
            let mut left = x + self.px(PAD);
            let mut room = w - 2 * self.px(PAD) - if close { self.px(CLOSE_SIDE) } else { 0 };
            if tab.dirty {
                p.text(clamp32(left), ty, "\u{25CF}", DIRTY_FG);
                (left, room) = (left + self.px(DOT), room - self.px(DOT));
            }
            let name = tabs::shorten_to(&tabs::label(&tab.path), |s| i64::from(p.text_width(s)) <= room);
            p.text(clamp32(left), ty, &name, TAB_FG);
            if close {
                let cross = p.text_width("\u{00D7}");
                p.text(clamp32(x + w - self.px(CLOSE_EDGE) - (self.px(CLOSE_SIDE) + i64::from(cross)) / 2), ty, "\u{00D7}", TAB_FG);
            }
        }
        // The full path of the tab under the pointer, just under the strip.
        if let Some(i) = self.tab_hover
            && let Some(tab) = self.tabs.get(i)
        {
            let text = tab.path.to_string_lossy();
            let (tw, th) = (i64::from(p.text_width(&text)), i64::from(p.line_height()));
            let (bw, bx) = (tw + 2 * self.px(6), (i as i64 * w).min((self.client_w - tw - 2 * self.px(6)).max(0)));
            p.fill_rect(rect(bx, bar + self.px(2), bw, th + 2 * self.px(3)), TIP_BG);
            p.text(clamp32(bx + self.px(6)), clamp32(bar + self.px(5)), &text, TIP_FG);
        }
    }
}
