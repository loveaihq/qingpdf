//! The command bar as the reader draws and uses it (2026-10-21): the buttons (the sidebar, open, save, print; select, highlight, note, draw, shapes, text
//! box, redact), the search pill and the "⋯" button of the menus. Where everything is and what a button needs is `crate::cmdbar`; this draws it, in the
//! accent colour for the tool in use, and does what a button does.

use super::overlay::{Hot, rc};
use super::*;
use crate::cmdbar::{self, BUTTONS, Kind};
use crate::ui::{Font, MenuEntry, SearchBox};

impl App {
    /// The height of the command bar, in pixels (0 when it is hidden: View > Command bar, full screen, the speed test).
    pub(super) fn cmd_h(&self) -> i64 {
        if self.toolbar_visible() { self.px(i64::from(cmdbar::BAR_H)) } else { 0 }
    }

    /// Where everything in the command bar is, from the top left corner of the bar.
    pub(super) fn cmd_layout(&self) -> cmdbar::Layout {
        cmdbar::layout(clamp32(self.client_w), self.dpi)
    }

    pub(super) fn cmd_inputs(&self) -> cmdbar::Inputs {
        cmdbar::Inputs { has_doc: self.doc.is_some(), can_edit: self.doc.is_some() && self.rights.annotate, tool: self.tool_command(), sidebar: self.sidebar_width() > 0 }
    }

    /// The words of the search pill's count ("3 / 12", with how far the search has got while it runs), when there is one.
    fn search_count(&self) -> String {
        match &self.search {
            Some(s) if self.find_open && s.total() > 0 => i18n::found(self.lang, s.current.map_or(0, |c| s.number_of(c)), s.total(), s.limit, None),
            Some(s) if self.find_open && s.running => t(self.lang, Msg::FindSearching).to_string(),
            Some(_) if self.find_open && !self.find_text.trim().is_empty() => t(self.lang, Msg::FindNone).to_string(),
            _ => String::new(),
        }
    }

    /// How wide the room for the count is, in pixels (the box the words are typed in is what is left).
    fn search_count_w(&self) -> i32 {
        let n = self.search_count().chars().count() as i64;
        if n == 0 { 0 } else { clamp32((n * i64::from(self.digit_w.max(1))).min(self.px(150))) }
    }

    /// The box the words to find are typed in, in the coordinates of the pages' area (the window moves it down); `None` when the bar is not shown.
    pub(super) fn search_box(&self) -> Option<SearchBox> {
        let pill = self.cmd_layout().search.filter(|_| self.toolbar_visible())?;
        let parts = cmdbar::search_parts(pill, self.dpi, self.search_count_w(), clamp32(self.px(18)));
        let up = clamp32(self.bar_h() - self.title_h());
        Some(SearchBox { rect: Rect { y: parts.edit.y - up, ..parts.edit }, hint: t(self.lang, Msg::SearchHint).to_string(), enabled: self.doc.is_some() })
    }

    /// What a button of the bar does: the shapes open a list of the four shapes, the others are the command of their menu item.
    pub(super) fn cmd_activate(&mut self, i: usize) -> Vec<Action> {
        let Some(d) = BUTTONS.get(i) else { return Vec::new() };
        if !cmdbar::button_state(d, &self.cmd_inputs()).0 {
            return Vec::new();
        }
        if d.kind == Kind::Shapes {
            let Some((_, r)) = self.cmd_layout().buttons.iter().find(|(n, _)| *n == i).copied() else { return Vec::new() };
            let tool = self.tool_command();
            let entries = cmdbar::SHAPES.iter().map(|&(id, m)| MenuEntry::Item { id, label: t(self.lang, m).to_string(), checked: tool == id, radio: true, enabled: true }).collect();
            // (In the coordinates of the pages' area: under the button.)
            let at = (r.x, clamp32(self.title_h() - self.bar_h()) + r.y + r.h);
            return vec![Action::ContextMenu { entries, at }];
        }
        self.run_command(d.cmd)
    }

    // --- drawing ---------------------------------------------------------------------------------------------------

    /// The picture of the shapes button: the shape that is the tool (a square when none is), drawn as thin lines in the middle of `r`.
    fn shape_icon(&self, p: &mut dyn Painter, r: Rect, color: Rgb) {
        let k = self.dpi as f32 / 96.0;
        let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
        let (s, w) = (5.5 * k, 1.5 * k);
        match self.tool_command() {
            CMD_TOOL_ELLIPSE => {
                let ring: Vec<(f32, f32)> = (0..=24).map(|i| (cx + s * (i as f32 * std::f32::consts::TAU / 24.0).cos(), cy + s * (i as f32 * std::f32::consts::TAU / 24.0).sin())).collect();
                p.polyline(&ring, w, color, 255);
            }
            CMD_TOOL_LINE => p.polyline(&[(cx - s, cy + s), (cx + s, cy - s)], w, color, 255),
            CMD_TOOL_ARROW => {
                p.polyline(&[(cx - s, cy + s), (cx + s, cy - s)], w, color, 255);
                p.polyline(&[(cx + 0.1 * s, cy - s), (cx + s, cy - s), (cx + s, cy - 0.1 * s)], w, color, 255);
            }
            _ => p.polyline(&[(cx - s, cy - s), (cx + s, cy - s), (cx + s, cy + s), (cx - s, cy + s), (cx - s, cy - s)], w, color, 255),
        }
    }

    /// The picture of the redact button: a black bar, as it will be on the page (the letter of the icon font is a block too big for the others).
    fn redact_icon(&self, p: &mut dyn Painter, r: Rect, color: Rgb) {
        let (w, h) = (clamp32(self.px(14)), clamp32(self.px(7)));
        p.fill_shape(Rect { x: r.x + (r.w - w) / 2, y: r.y + (r.h - h) / 2, w, h }, [clamp32(self.px(2)); 4], color, 255);
    }

    pub(super) fn draw_cmdbar(&self, p: &mut dyn Painter) {
        let (title, h) = (self.title_h(), self.cmd_h());
        if h == 0 {
            return;
        }
        let l = self.cmd_layout();
        let pal = &self.pal;
        let down = |r: Rect| Rect { y: r.y + clamp32(title), ..r };
        p.fill_rect(rc(0, title, self.client_w, h), pal.surface);
        p.fill_rect(rc(0, title + h - 1, self.client_w, 1), pal.hairline);
        let inputs = self.cmd_inputs();
        let radius = clamp32(self.px(6));
        for (i, r) in &l.buttons {
            let Some(d) = BUTTONS.get(*i) else { continue };
            let (enabled, active) = cmdbar::button_state(d, &inputs);
            let r = down(*r);
            let pressed = self.pressed == Some(Hot::Cmd(*i)) && self.hot == self.pressed;
            let ink = if !enabled {
                pal.disabled_fg
            } else if active {
                pal.accent_on_tint
            } else {
                pal.status_fg
            };
            if active {
                p.fill_shape(r, [radius; 4], pal.accent_tint, 255);
            }
            let level = self.hover_level(Hot::Cmd(*i));
            if enabled && level > 0 {
                self.hover_fill(p, r, radius, pressed, level);
            }
            if d.kind == Kind::Shapes {
                self.shape_icon(p, r, ink);
            } else if d.cmd == CMD_TOOL_REDACT {
                self.redact_icon(p, r, ink);
            } else {
                p.glyph(r, d.glyph, ink);
            }
        }
        for s in &l.seps {
            p.fill_rect(down(*s), pal.hairline);
        }
        // The search pill: a glass, the box the words are typed in (a window of its own over it, with the same fill), and the count.
        if let Some(pill) = l.search {
            let pill = down(pill);
            let pr = pill.h / 2;
            p.fill_shape(pill, [pr; 4], pal.field_bg, 255);
            if self.find_open {
                p.stroke_round(pill, pr, clamp32(self.px(2)), pal.accent, 255);
            } else {
                p.stroke_round(pill, pr, 1, if self.hot == Some(Hot::Search) { pal.dim_fg } else { pal.hairline }, 255);
            }
            let parts = cmdbar::search_parts(pill, self.dpi, self.search_count_w(), clamp32(self.px(18)));
            p.glyph(parts.icon, '\u{E721}', if self.doc.is_some() { pal.dim_fg } else { pal.disabled_fg });
            if let Some(c) = parts.count {
                p.set_font(Font::Body);
                let text = self.search_count();
                let tw = p.text_width(&text);
                p.text((c.x + c.w - tw).max(c.x), c.y + (c.h - p.line_height()) / 2, &text, pal.dim_fg);
            }
        }
        let more = down(l.more);
        let more_pressed = self.pressed == Some(Hot::More) && self.hot == self.pressed;
        let more_level = self.hover_level(Hot::More);
        if more_level > 0 {
            self.hover_fill(p, more, radius, more_pressed, more_level);
        }
        p.glyph(more, '\u{E712}', pal.status_fg);
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::{Rec, open_pages};
    use super::*;

    fn app() -> App {
        app_named("cmdbar")
    }

    fn app_named(name: &str) -> App {
        let mut app = open_pages(name, &[(612, 792, 0); 4], false);
        app.bare = false;
        app.event(Event::Size { w: 1400, h: 900 });
        app
    }

    fn click_button(app: &mut App, cmd: u32) -> Vec<Action> {
        let l = app.cmd_layout();
        let r = l.buttons.iter().find(|(i, _)| BUTTONS[*i].cmd == cmd).unwrap().1;
        let (x, y) = (r.x + r.w / 2, app.title_h() as i32 + r.y + r.h / 2);
        app.event(Event::Hover { x, y });
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x, y })
    }

    #[test]
    fn the_command_bar_is_always_there_below_the_title_strip_and_the_pages_start_below_it() {
        let app = app();
        assert_eq!(app.title_h(), 40);
        assert_eq!(app.cmd_h(), 44);
        assert_eq!((app.bar_h(), app.view_h), (84, 900 - 84));
        // Hidden (View > Command bar), the pages start under the title strip.
        let mut app = app;
        app.event(Event::Command(CMD_TOOLBAR));
        assert_eq!((app.bar_h(), app.view_h), (40, 860));
        app.event(Event::Command(CMD_TOOLBAR));
        assert_eq!(app.bar_h(), 84);
    }

    #[test]
    fn a_button_acts_when_the_mouse_comes_up_on_it_and_not_when_it_comes_up_elsewhere() {
        let mut app = app_named("cmdbar-click");
        let actions = click_button(&mut app, CMD_TOOL_INK);
        assert_eq!(app.edit.tool, Tool::Ink);
        assert!(actions.iter().any(|a| matches!(a, Action::Invalidate)));
        // A second click puts the tool down again.
        click_button(&mut app, CMD_TOOL_INK);
        assert_eq!(app.edit.tool, Tool::Select);
        // Pressed on a button and let go somewhere else: nothing is done.
        let l = app.cmd_layout();
        let r = l.buttons.iter().find(|(i, _)| BUTTONS[*i].cmd == CMD_TOOL_NOTE).unwrap().1;
        let (x, y) = (r.x + 5, app.title_h() as i32 + r.y + 5);
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: x + 300, y: y + 400, shift: false });
        app.event(Event::MouseUp { x: x + 300, y: y + 400 });
        assert_eq!(app.edit.tool, Tool::Select);
        assert!(app.press.is_none(), "the page heard nothing of it");
    }

    #[test]
    fn open_and_the_sidebar_and_print_are_buttons_too() {
        let mut app = app_named("cmdbar-open");
        let actions = click_button(&mut app, CMD_OPEN);
        assert!(actions.iter().any(|a| matches!(a, Action::PickFile { .. })));
        let shown = app.sidebar_width() > 0;
        click_button(&mut app, CMD_BOOKMARKS);
        assert_ne!(app.sidebar_width() > 0, shown);
        let actions = click_button(&mut app, CMD_PRINT);
        assert!(actions.iter().any(|a| matches!(a, Action::ChoosePrinter { .. })));
    }

    #[test]
    fn the_shapes_button_opens_a_list_of_the_four_shapes_under_it() {
        let mut app = app_named("cmdbar-shapes");
        let l = app.cmd_layout();
        let (i, r) = l.buttons.iter().find(|(i, _)| BUTTONS[*i].kind == Kind::Shapes).copied().unwrap();
        let (x, y) = (r.x + 3, app.title_h() as i32 + r.y + 3);
        app.event(Event::Hover { x, y });
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        let actions = app.event(Event::MouseUp { x, y });
        let (entries, at) = actions.iter().find_map(|a| if let Action::ContextMenu { entries, at } = a { Some((entries, *at)) } else { None }).expect("a list");
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], MenuEntry::Item { id, .. } if *id == CMD_TOOL_RECT));
        // The list is under the button: in the window's pixels, its top is the button's bottom.
        assert_eq!(at, (r.x, app.title_h() as i32 + r.y + r.h));
        let _ = i;
        // Choosing one makes it the tool, and the button is pressed from then on.
        app.event(Event::Command(CMD_TOOL_ELLIPSE));
        assert_eq!(app.tool_command(), CMD_TOOL_ELLIPSE);
        assert!(cmdbar::button_state(&BUTTONS[i], &app.cmd_inputs()).1);
    }

    #[test]
    fn a_button_that_cannot_be_used_does_nothing() {
        let mut app = App::new(Lang::En, Arc::new(|| {}), std::time::Instant::now(), 1920 * 1080, "Tester".to_string());
        app.persist = false;
        app.recent_path = None;
        app.bare = false;
        app.event(Event::Size { w: 1400, h: 900 });
        click_button(&mut app, CMD_TOOL_INK);
        assert_eq!(app.edit.tool, Tool::Select);
        assert!(!cmdbar::button_state(&BUTTONS[7], &app.cmd_inputs()).0);
        // Open works without a file.
        let actions = click_button(&mut app, CMD_OPEN);
        assert!(actions.iter().any(|a| matches!(a, Action::PickFile { .. })));
    }

    #[test]
    fn the_menu_button_asks_for_the_menus_under_it_and_alt_asks_for_them_with_the_keyboard() {
        let mut app = app_named("cmdbar-menu");
        let l = app.cmd_layout();
        let more = l.more;
        let (x, y) = (more.x + 5, app.title_h() as i32 + more.y + 5);
        app.event(Event::Hover { x, y });
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        let actions = app.event(Event::MouseUp { x, y });
        let (at, keyboard) = actions.iter().find_map(|a| if let Action::ShowMenu { at, keyboard } = a { Some((*at, *keyboard)) } else { None }).expect("the menus");
        assert!(!keyboard);
        assert_eq!(at, (more.x + more.w, app.title_h() as i32 + more.y + more.h), "under the button, its right edge");
        let actions = app.event(Event::MenuKey);
        assert!(actions.iter().any(|a| matches!(a, Action::ShowMenu { keyboard: true, .. })));
        // The menus are still all there for the window to show.
        assert_eq!(app.menus().len(), 10);
    }

    #[test]
    fn the_search_box_is_told_to_the_window_inside_the_pill_and_the_count_makes_room_for_itself() {
        let mut app = app_named("cmdbar-search");
        let first = app.event(Event::Size { w: 1400, h: 900 });
        let sb = first.iter().rev().find_map(|a| if let Action::SearchBox(Some(b)) = a { Some(b.clone()) } else { None });
        let l = app.cmd_layout();
        let pill = l.search.unwrap();
        // (It is not said again when nothing changed; the first time it was said when the file was opened.)
        let b = sb.or_else(|| app.search_box()).unwrap();
        assert!(b.enabled && b.hint == "Find in this file");
        assert!(b.rect.x > pill.x && b.rect.x + b.rect.w < pill.x + pill.w && b.rect.h <= pill.h);
        // In the pages' area's coordinates: the window puts it down by the title strip and the bar.
        assert_eq!(i64::from(b.rect.y) + app.bar_h(), app.title_h() + i64::from(pill.y) + i64::from((pill.h - b.rect.h) / 2));
        // The command bar hidden: no box (the find box lies over the pages).
        app.event(Event::Command(CMD_TOOLBAR));
        assert!(app.search_box().is_none());
    }

    #[test]
    fn the_count_of_the_search_is_written_in_the_pill_and_takes_room_from_the_box_to_type_in() {
        let mut app = App::new(Lang::En, Arc::new(|| {}), std::time::Instant::now(), 1920 * 1080, "Tester".to_string());
        app.persist = false;
        app.recent_path = None;
        app.bare = false;
        app.event(Event::Size { w: 1400, h: 900 });
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world.pdf");
        app.open_path(std::path::Path::new(corpus), "");
        super::super::sidebar::tests::pump(&mut app, "the file to open", |a| a.has_document());
        let before = app.search_box().expect("a box").rect;
        assert_eq!(app.search_count(), "");
        // The keyboard goes into the box (it is the find box now), and what is typed is looked for.
        app.event(Event::FindFocus);
        app.event(Event::FindText("world".to_string()));
        app.event(Event::Timer(TIMER_FIND));
        super::super::sidebar::tests::pump(&mut app, "the search to end", |a| a.search.as_ref().is_some_and(|s| !s.running && s.total() > 0));
        assert_eq!(app.search_count(), "1 / 2");
        let after = app.search_box().expect("a box").rect;
        assert!(after.w < before.w && after.x == before.x && after.y == before.y, "{before:?} {after:?}");
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.texts.iter().any(|(_, _, s)| s == "1 / 2"), "{:?}", rec.texts);
        // Nothing found: the pill says so; the box is closed by Esc and the count goes.
        app.event(Event::FindText("zzzz".to_string()));
        app.event(Event::Timer(TIMER_FIND));
        super::super::sidebar::tests::pump(&mut app, "the second search to end", |a| a.search.as_ref().is_some_and(|s| !s.running && s.total() == 0));
        assert_eq!(app.search_count(), t(Lang::En, Msg::FindNone));
        app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
        assert!(!app.find_open && app.search_count().is_empty());
    }

    #[test]
    fn the_bar_is_drawn_with_the_tool_in_use_on_the_tint_and_every_button_has_its_picture() {
        let mut app = app_named("cmdbar-draw");
        app.event(Event::Command(CMD_TOOL_INK));
        let mut rec = Rec::default();
        app.paint(&mut rec);
        // The icon font's letters of the buttons are drawn as text (the test painter writes down text); the ink button's tint is the accent's.
        for d in &BUTTONS {
            if d.kind != Kind::Shapes && d.cmd != CMD_TOOL_REDACT {
                assert!(rec.texts.iter().any(|(_, _, s)| s == &d.glyph.to_string()), "no picture for {:?}", d.glyph);
            }
        }
        assert!(rec.fills.iter().any(|(_, c)| *c == app.pal.accent_tint), "the tool in use is on the tint");
        assert_eq!(accent::QING_LIGHT.tint, app.pal.accent_tint);
    }
}
