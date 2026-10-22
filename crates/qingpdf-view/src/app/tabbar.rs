//! The title strip (2026-10-21; before that, the strip of tabs, shown from two files on): at the top of the window, always, in the place of the system's
//! title bar. From the left: the mark, the tabs (one for each file, the shown one raised, with a line of the accent colour on top; each says the name of
//! its file, has a dot in the accent colour when the file has changes not saved, and a cross to close it), the "+" that opens a file, and what is left is
//! where the window is dragged. The system draws its buttons (minimize, maximize, close) over the right end. Where the parts are is plain arithmetic in `tabs.rs`.

use super::overlay::{Hot, draw_logo, rc};
use super::*;
use crate::tabs::{self, Strip, WIDE_TAB};
use crate::ui::{Font, Rgb};

/// The painter of the pages when something is above them: everything is drawn `dy` lower.
pub(super) struct Shifted<'a> {
    pub(super) inner: &'a mut dyn Painter,
    pub(super) dx: i32,
    pub(super) dy: i32,
}

impl Shifted<'_> {
    fn down(&self, r: Rect) -> Rect {
        Rect { x: r.x.saturating_add(self.dx), y: r.y.saturating_add(self.dy), ..r }
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
        let (x, y) = (x.saturating_add(self.dx), y.saturating_add(self.dy));
        self.inner.text(x, y, s, color);
    }

    fn text_width(&mut self, s: &str) -> i32 {
        self.inner.text_width(s)
    }

    fn line_height(&self) -> i32 {
        self.inner.line_height()
    }

    fn set_font(&mut self, font: Font) {
        self.inner.set_font(font);
    }

    fn fill_shape(&mut self, r: Rect, radii: [i32; 4], color: Rgb, alpha: u8) {
        let r = self.down(r);
        self.inner.fill_shape(r, radii, color, alpha);
    }

    fn stroke_round(&mut self, r: Rect, radius: i32, width: i32, color: Rgb, alpha: u8) {
        let r = self.down(r);
        self.inner.stroke_round(r, radius, width, color, alpha);
    }

    fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Rgb, alpha: u8) {
        let (dx, dy) = (self.dx as f32, self.dy as f32);
        let moved: Vec<(f32, f32)> = points.iter().map(|&(x, y)| (x + dx, y + dy)).collect();
        self.inner.polyline(&moved, width, color, alpha);
    }

    fn glyph(&mut self, r: Rect, ch: char, color: Rgb) {
        let r = self.down(r);
        self.inner.glyph(r, ch, color);
    }
}

impl App {
    /// The height of the title strip, in pixels (0 in full screen and in the speed test).
    pub(super) fn title_h(&self) -> i64 {
        if self.full || self.bench.is_some() || self.bare { 0 } else { self.px(tabs::TITLE_H) }
    }

    /// The width of the system's buttons at the right end of the strip, in pixels (none when the system has its own title bar above the window).
    fn caption_px(&self) -> i64 {
        if self.frame_custom { self.caption_w } else { 0 }
    }

    /// Where the parts of the strip are.
    pub(super) fn strip(&self) -> Strip {
        Strip::new(self.tabs.len(), self.dpi, self.client_w - self.caption_px() - self.px(tabs::DRAG_MIN))
    }

    /// Does tab `i` show its close button? The shown one does, the one under the pointer does, and so does any that is wide enough.
    pub(super) fn shows_close(&self, i: usize) -> bool {
        self.tabs.active() == Some(i) || matches!(self.hot, Some(Hot::Tab(n) | Hot::Close(n)) if n == i) || self.strip().width >= self.px(WIDE_TAB)
    }

    /// What the frame of the window should treat this place as: the empty part of the title strip is the title bar (dragged, and a double click makes the
    /// window big or small); the rest is the reader's.
    pub(super) fn nc_at(&self, x: i64, y: i64) -> crate::ui::NcHit {
        use crate::ui::NcHit;
        if !self.frame_custom || y < 0 || y >= self.title_h() {
            return NcHit::Client;
        }
        match tabs::hit(&self.strip(), x, y, |i| self.shows_close(i)) {
            tabs::Hit::Nothing => NcHit::Caption,
            _ => NcHit::Client,
        }
    }

    /// The middle button came up in the strip: it closes the tab under it.
    pub(super) fn strip_middle(&mut self, x: i64, y: i64) -> Vec<Action> {
        match tabs::hit(&self.strip(), x, y, |i| self.shows_close(i)) {
            tabs::Hit::Tab(i) | tabs::Hit::Close(i) => self.close_tab(i),
            _ => Vec::new(),
        }
    }

    /// The shape of a tab: the rectangle with its top corners rounded.
    fn tab_radii(&self) -> [i32; 4] {
        let r = clamp32(self.px(8));
        [r, r, 0, 0]
    }

    /// Draw the title strip over the top of the window.
    pub(super) fn draw_title(&self, p: &mut dyn Painter) {
        let h = self.title_h();
        if h == 0 {
            return;
        }
        let pal = &self.pal;
        let strip = self.strip();
        p.fill_rect(rc(0, 0, self.client_w, h), pal.strip_bg);
        let (mx, my, ms) = strip.mark;
        draw_logo(p, rc(mx, my, ms, ms));
        p.set_font(Font::Body);
        let lh = i64::from(p.line_height());
        if self.tabs.is_empty() {
            p.set_font(Font::Bold);
            let ty = (h - i64::from(p.line_height())) / 2;
            p.text(clamp32(strip.first), clamp32(ty), "qingpdf", pal.status_fg);
            p.set_font(Font::Body);
            return;
        }
        let (pad, dot) = (self.px(10), self.px(12));
        let radii = self.tab_radii();
        for (i, tab) in self.tabs.iter().enumerate() {
            let x = strip.xs.get(i).copied().unwrap_or(0) + self.tab_offset(i);
            let shown = self.tabs.active() == Some(i);
            let hot = matches!(self.hot, Some(Hot::Tab(n) | Hot::Close(n)) if n == i);
            let hover = self.hover_level(Hot::Tab(i)).max(self.hover_level(Hot::Close(i)));
            // The shown tab is the surface of the command bar, a little taller than the strip so that it flows into it, with the accent on its top edge.
            if shown {
                let body = rc(x, strip.top, strip.width, strip.height + 1);
                p.fill_shape(body, radii, pal.accent, 255);
                p.fill_shape(rc(x, strip.top + self.px(2), strip.width, strip.height + 1 - self.px(2)), radii, pal.tab_on, 255);
            } else if hover > 0 {
                let body = rc(x, strip.top + self.px(2), strip.width, strip.height - self.px(4));
                self.hover_fill(p, body, clamp32(self.px(8)), false, hover);
            }
            let close = self.shows_close(i);
            let mut left = x + pad;
            let mut room = strip.width - 2 * pad - if close { strip.close.0 + self.px(2) } else { 0 };
            let ty = strip.top + (strip.height - lh) / 2;
            if tab.dirty {
                let d = self.px(8);
                p.fill_shape(rc(left, strip.top + (strip.height - d) / 2, d, d), [clamp32(d / 2); 4], pal.dirty_fg, 255);
                (left, room) = (left + dot, room - dot);
            }
            let name = tabs::shorten_to(&tabs::label(&tab.path), |s| i64::from(p.text_width(s)) <= room);
            p.text(clamp32(left), clamp32(ty), &name, if shown || hot { pal.tab_fg } else { pal.dim_fg });
            if close && let Some((cl, ct, side)) = strip.close_box(i) {
                let r = rc(cl, ct, side, side);
                let on = self.hot == Some(Hot::Close(i));
                let level = self.hover_level(Hot::Close(i));
                if level > 0 || self.pressed == Some(Hot::Close(i)) {
                    self.hover_fill(p, r, clamp32(self.px(6)), self.pressed == Some(Hot::Close(i)) && on, level);
                }
                let (cx, cy, a) = (cl as f32 + side as f32 / 2.0, ct as f32 + side as f32 / 2.0, side as f32 * 0.23);
                let w = (1.4 * self.dpi as f32 / 96.0).max(1.0);
                let ink = if shown || hot { pal.status_fg } else { pal.dim_fg };
                p.polyline(&[(cx - a, cy - a), (cx + a, cy + a)], w, ink, 255);
                p.polyline(&[(cx + a, cy - a), (cx - a, cy + a)], w, ink, 255);
            }
        }
        if let Some((l, t, side)) = strip.plus {
            let r = rc(l, t, side, side);
            let on = self.hot == Some(Hot::Plus);
            let level = self.hover_level(Hot::Plus);
            if level > 0 || self.pressed == Some(Hot::Plus) {
                self.hover_fill(p, r, clamp32(self.px(6)), self.pressed == Some(Hot::Plus) && on, level);
            }
            let (cx, cy, a) = (l as f32 + side as f32 / 2.0, t as f32 + side as f32 / 2.0, side as f32 * 0.21);
            let w = (1.5 * self.dpi as f32 / 96.0).max(1.0);
            p.polyline(&[(cx - a, cy), (cx + a, cy)], w, pal.status_fg, 255);
            p.polyline(&[(cx, cy - a), (cx, cy + a)], w, pal.status_fg, 255);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::Rec;
    use super::*;

    #[test]
    fn the_shifted_painter_moves_everything_down() {
        let mut rec = Rec::default();
        {
            let mut s = Shifted { inner: &mut rec, dx: 0, dy: 30 };
            s.fill_rect(Rect { x: 1, y: 2, w: 3, h: 4 }, 5);
            s.fill_shape(Rect { x: 1, y: 2, w: 3, h: 4 }, [1; 4], 6, 255);
            s.text(7, 8, "x", 9);
            s.glyph(Rect { x: 0, y: 10, w: 20, h: 20 }, 'a', 1);
        }
        assert_eq!(rec.fills[0].0.y, 32);
        assert_eq!(rec.fills[1].0.y, 32);
        assert!(rec.texts.iter().any(|(x, y, s)| (*x, *y, s.as_str()) == (7, 38, "x")));
        // The letter of the icon font is put by the painter under it, at the place moved.
        assert!(rec.texts.iter().any(|(_, y, s)| s == "a" && *y >= 40));
    }
}
