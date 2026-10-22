//! The welcome page as the reader draws and uses it (2026-10-21): shown while no file is open. The mark and the name, the line under it, the button that
//! opens a file, the hint that a file can be dropped on the window, and the files opened last, each a row to click or to reach with the arrow keys and open
//! with Enter or take out of the list with Delete. Where everything is is `crate::welcome`.

use super::overlay::{Hot, draw_logo, rc};
use super::*;
use crate::accent;
use crate::tabs;
use crate::ui::Font;
use crate::welcome::{self, Key};

impl App {
    /// The welcome page is shown while there is no file (and none is being opened).
    pub(super) fn welcome_shown(&self) -> bool {
        self.tabs.is_empty() && self.doc.is_none() && self.present.is_none() && self.bench.is_none()
    }

    /// Where everything on the page is, in the pages' area.
    pub(super) fn welcome_layout(&self) -> welcome::Layout {
        let (title, line) = (self.title_font_h.max(1), self.line_h.max(1));
        welcome::layout(rc(self.view_x, 0, self.view_w.max(self.client_w - self.view_x), self.view_h), self.dpi, self.recent.len(), title, line)
    }

    /// A key on the welcome page: the arrow keys choose a row, Enter opens it, Delete takes it out of the list. `None` for any other key.
    pub(super) fn welcome_key(&mut self, key: u32) -> Option<Vec<Action>> {
        if !self.welcome_shown() {
            return None;
        }
        let shown = self.welcome_layout().rows.len().min(self.recent.len());
        let step = match key {
            vk::UP => Some(Key::Up),
            vk::DOWN => Some(Key::Down),
            vk::HOME => Some(Key::Home),
            vk::END => Some(Key::End),
            _ => None,
        };
        if let Some(step) = step {
            self.welcome_focus = welcome::step_focus(self.welcome_focus.filter(|&i| i < shown), shown, step);
            return Some(vec![Action::Invalidate]);
        }
        match key {
            vk::RETURN => Some(match self.welcome_focus.filter(|&i| i < shown) {
                Some(i) => self.open_recent(i),
                None => self.run_command(CMD_OPEN),
            }),
            vk::DELETE => {
                // The row the keyboard is on, else the one the pointer is on.
                let hovered = if let Some(Hot::Row(i)) = self.hot { Some(i) } else { None };
                let i = self.welcome_focus.filter(|&i| i < shown).or(hovered)?;
                let path = self.recent.get(i)?.path.clone();
                recent::forget(&mut self.recent, &path);
                self.save_recent();
                self.menus_dirty = true;
                self.welcome_focus = welcome::focus_after_removal(i, self.recent.len().min(shown));
                self.hot = None;
                Some(vec![Action::Invalidate])
            }
            _ => None,
        }
    }

    pub(super) fn draw_welcome(&self, p: &mut dyn Painter) {
        let pal = &self.pal;
        let l = self.welcome_layout();
        draw_logo(p, l.logo);
        let cx = l.logo.x + l.logo.w / 2;
        let centered = |p: &mut dyn Painter, y: i32, s: &str, color: Rgb| {
            let w = p.text_width(s);
            p.text(cx - w / 2, y, s, color);
        };
        p.set_font(Font::Title);
        centered(p, l.title_y, "qingpdf", pal.status_fg);
        p.set_font(Font::Body);
        centered(p, l.tagline_y, t(self.lang, Msg::WelcomeTagline), pal.dim_fg);
        // The button that opens a file: the accent, a darker one under the pointer.
        let (hot, pressed) = (self.hot == Some(Hot::Open), self.pressed == Some(Hot::Open) && self.hot == Some(Hot::Open));
        let fill = if hot || pressed { pal.accent_hover } else { pal.accent };
        p.fill_shape(l.button, [clamp32(self.px(8)); 4], fill, 255);
        p.set_font(Font::Bold);
        let label = t(self.lang, Msg::WelcomeOpen);
        let (w, h) = (p.text_width(label), p.line_height());
        p.text(l.button.x + (l.button.w - w) / 2, l.button.y + (l.button.h - h) / 2, label, pal.on_accent);
        p.set_font(Font::Body);
        centered(p, l.hint_y, t(self.lang, Msg::WelcomeDrop), pal.dim_fg);
        if l.card.h == 0 {
            return;
        }
        // The list: a heading with what Delete does at its right end, and a card with the rows on it.
        p.set_font(Font::Bold);
        p.text(l.card.x + clamp32(self.px(6)), l.heading_y, t(self.lang, Msg::WelcomeRecent), pal.status_fg);
        p.set_font(Font::Body);
        if !self.recent.is_empty() {
            let note = t(self.lang, Msg::WelcomeRemove);
            let nw = p.text_width(note);
            p.text(l.card.x + l.card.w - nw - clamp32(self.px(6)), l.heading_y, note, pal.hint_fg);
        }
        let radius = clamp32(self.px(i64::from(welcome::CARD_RADIUS)));
        p.fill_shape(Rect { x: l.card.x, y: l.card.y + 1, w: l.card.w, h: l.card.h }, [radius; 4], 0x000000, pal.shadow_alpha / 2);
        p.fill_shape(l.card, [radius; 4], pal.surface, 255);
        p.stroke_round(l.card, radius, 1, pal.hairline, 255);
        if self.recent.is_empty() {
            if let Some(r) = l.rows.first() {
                let s = t(self.lang, Msg::WelcomeNone);
                let (w, h) = (p.text_width(s), p.line_height());
                p.text(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, s, pal.hint_fg);
            }
            return;
        }
        let today = (self.pn.today)();
        let row_radius = clamp32(self.px(8));
        let pad = clamp32(self.px(14));
        for (i, (r, entry)) in l.rows.iter().zip(&self.recent).enumerate() {
            let is_hot = self.hot == Some(Hot::Row(i));
            let focused = self.welcome_focus == Some(i);
            if is_hot || focused {
                self.hover_fill(p, *r, row_radius, self.pressed == Some(Hot::Row(i)) && is_hot, 255);
            }
            if focused {
                p.stroke_round(*r, row_radius, clamp32(self.px(2)), pal.accent, 255);
            }
            let (name, folder) = welcome::split_path(&entry.path);
            let day = welcome::day_text(self.lang, welcome::relative(&today, &entry.date));
            let (dw, lh) = (p.text_width(&day), p.line_height());
            let room = i64::from(r.w) - 2 * i64::from(pad) - i64::from(dw) - self.px(12);
            p.set_font(Font::Bold);
            let name = tabs::shorten_to(name, |s| i64::from(p.text_width(s)) <= room);
            p.text(r.x + pad, r.y + clamp32(self.px(8)), &name, pal.status_fg);
            p.set_font(Font::Body);
            p.text(r.x + r.w - pad - dw, r.y + clamp32(self.px(8)), &day, pal.dim_fg);
            let folder_room = i64::from(r.w) - 2 * i64::from(pad);
            let folder = tabs::shorten_to(folder, |s| i64::from(p.text_width(s)) <= folder_room);
            p.text(r.x + pad, r.y + r.h - clamp32(self.px(8)) - lh, &folder, pal.hint_fg);
        }
        let _ = accent::QING_LIGHT;
    }

}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::Rec;
    use super::*;
    use crate::recent::Entry;

    fn app() -> App {
        let mut app = App::new(Lang::En, Arc::new(|| {}), std::time::Instant::now(), 1920 * 1080, "Tester".to_string());
        app.persist = false;
        app.recent_path = None;
        app.recent = vec![Entry { date: "2026-10-21".to_string(), ..Entry::new("C:\\docs\\annual report.pdf") }, Entry { date: "2026-10-20".to_string(), ..Entry::new("D:\\books\\一本书.pdf") }, Entry::new("C:\\x\\y.pdf")];
        app.set_clock(|| "2026-10-21".to_string());
        app.bare = false;
        app.event(Event::Size { w: 1400, h: 900 });
        app
    }

    fn click(app: &mut App, r: Rect) -> Vec<Action> {
        let (x, y) = (r.x + r.w / 2, app.bar_h() as i32 + r.y + r.h / 2);
        app.event(Event::Hover { x, y });
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x, y })
    }

    #[test]
    fn with_no_file_the_page_says_what_the_program_is_and_lists_the_files_opened_last() {
        let mut app = app();
        assert!(app.welcome_shown());
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let said = |s: &str| rec.texts.iter().any(|(_, _, t)| t == s);
        assert!(said("qingpdf") && said("Small, fast, offline PDF tool") && said("Open file") && said("or drop a PDF here"), "{:?}", rec.texts);
        // The files: name, folder, and the day as today, yesterday or nothing for a day not known.
        assert!(said("annual report.pdf") && said("C:\\docs") && said("Today") && said("Yesterday") && said("一本书.pdf") && said("y.pdf"), "{:?}", rec.texts);
        assert!(said("Recent") && said("Del removes from the list"));
    }

    #[test]
    fn in_chinese_the_page_is_in_chinese() {
        let mut app = app();
        app.event(Event::Command(CMD_LANG_ZH));
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let said = |s: &str| rec.texts.iter().any(|(_, _, t)| t == s);
        assert!(said("轻巧、快速、离线的 PDF 工具") && said("打开文件") && said("或把 PDF 拖到这里") && said("今天") && said("昨天") && said("最近打开"), "{:?}", rec.texts);
    }

    #[test]
    fn an_empty_list_says_so() {
        let mut app = app();
        app.recent.clear();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.texts.iter().any(|(_, _, t)| t == "No recent files yet"), "{:?}", rec.texts);
        assert!(!rec.texts.iter().any(|(_, _, t)| t == "Del removes from the list"));
    }

    #[test]
    fn the_button_asks_for_a_file_and_a_row_opens_its_file() {
        let mut app = app();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let l = app.welcome_layout();
        assert!(click(&mut app, l.button).iter().any(|a| matches!(a, Action::PickFile { .. })));
        // A row: the file is opened (it is not there, so nothing is shown, but the reader began to open it: a tab was made).
        let actions = click(&mut app, l.rows[1]);
        assert!(!actions.is_empty());
        assert!(app.tabs.len() == 1 || app.recent.iter().all(|e| e.path != "D:\\books\\一本书.pdf"), "the file was asked for");
    }

    #[test]
    fn the_arrow_keys_choose_a_row_enter_opens_it_and_delete_takes_it_out_of_the_list() {
        let mut app = app();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let key = |app: &mut App, vk: u32| app.event(Event::Key { vk, ctrl: false, shift: false });
        key(&mut app, vk::DOWN);
        assert_eq!(app.welcome_focus, Some(0));
        key(&mut app, vk::DOWN);
        key(&mut app, vk::DOWN);
        key(&mut app, vk::DOWN);
        assert_eq!(app.welcome_focus, Some(2), "it stops at the last row");
        key(&mut app, vk::UP);
        assert_eq!(app.welcome_focus, Some(1));
        // Delete takes the chosen row out of the list; the row after it is chosen now.
        key(&mut app, vk::DELETE);
        assert_eq!(app.recent.len(), 2);
        assert!(app.recent.iter().all(|e| !e.path.contains("一本书")));
        assert_eq!(app.welcome_focus, Some(1));
        key(&mut app, vk::DELETE);
        key(&mut app, vk::DELETE);
        assert!(app.recent.is_empty() && app.welcome_focus.is_none());
        // Enter with no row chosen asks for a file.
        let actions = key(&mut app, vk::RETURN);
        assert!(actions.iter().any(|a| matches!(a, Action::PickFile { .. })));
        // The keys do nothing special once a file is open (they scroll).
    }

    #[test]
    fn delete_takes_out_the_row_under_the_pointer_when_no_row_has_the_keyboard() {
        let mut app = app();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let l = app.welcome_layout();
        let r = l.rows[2];
        app.event(Event::Hover { x: r.x + 5, y: app.bar_h() as i32 + r.y + 5 });
        assert_eq!(app.hot, Some(Hot::Row(2)));
        app.event(Event::Key { vk: vk::DELETE, ctrl: false, shift: false });
        assert_eq!(app.recent.len(), 2);
        assert!(app.recent.iter().all(|e| !e.path.ends_with("y.pdf")));
    }

    #[test]
    fn the_cursor_is_a_hand_over_the_button_and_the_rows() {
        let mut app = app();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let l = app.welcome_layout();
        let at = |app: &mut App, r: Rect| app.cursor_at(r.x + 3, app.bar_h() as i32 + r.y + 3);
        assert_eq!(at(&mut app, l.button), Cursor::Hand);
        assert_eq!(at(&mut app, l.rows[0]), Cursor::Hand);
        assert_eq!(app.cursor_at(5, app.bar_h() as i32 + 5), Cursor::Arrow);
    }
}
