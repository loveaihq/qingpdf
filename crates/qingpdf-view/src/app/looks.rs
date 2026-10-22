//! How the pages go in rows, how the window looks and in what colours the pages are shown (2026-10-20): the commands of View > Page layout,
//! Interface and Page colours, and the little that the drawing asks of them. The arithmetic of the rows is `layout.rs`, the colours of the window
//! `theme.rs`, the colours of a page `qingpdf_core::render::recolor`.

use super::*;
use crate::ui::{Skin, ThemeSpec};

impl App {
    /// How the pages are in rows now: what the person chose for this file; one to a row while the presentation is going on.
    pub(super) fn arrangement(&self) -> Arrangement {
        if self.present.is_some() { Arrangement::default() } else { Arrangement { spread: self.spread, r2l: self.r2l } }
    }

    /// View > Page layout: this file's pages go in rows of one or two, and the next file with nothing kept about it does the same.
    pub(super) fn set_spread(&mut self, spread: Spread) -> Vec<Action> {
        self.default_spread = spread;
        self.keep(|s| s.layout = spread);
        self.menus_dirty = true;
        if self.doc.is_none() || self.present.is_some() || self.spread == spread {
            return Vec::new();
        }
        self.spread = spread;
        // The page in the middle of the window stays in view.
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        self.relayout(ax, ay, None);
        let actions = self.after_view_change(false);
        self.remember_position();
        actions
    }

    /// The window is told how it looks when the reader next answers (the frame, the boxes and the tree of bookmarks are Windows' own and are told the colours;
    /// the text it draws is told the language, which decides the fonts).
    pub(super) fn theme_spec(&self) -> ThemeSpec {
        let p = &self.pal;
        let skin = Skin { surface: p.surface, text: p.status_fg, dim: p.dim_fg, off: p.disabled_fg, line: p.hairline, field: p.field_bg, accent: p.accent, accent_hover: p.accent_hover, tint: p.accent_tint, on_tint: p.accent_on_tint, on_accent: p.on_accent, hover: p.status_fg };
        ThemeSpec { dark: self.dark, zh: self.lang == Lang::Zh, strip: p.strip_bg, text: p.status_fg, field_bg: p.field_bg, tree_bg: p.surface, tree_line: p.thumb_border, skin }
    }

    /// Take the colours that the choices and the system make now: a high-contrast theme's own, else light or dark with the accent (青, or the system's).
    pub(super) fn apply_theme(&mut self) {
        self.dark = match &self.contrast {
            Some(c) => accent::luminance(c.window) < 0.18,
            None => self.theme_choice.dark(self.system_dark),
        };
        self.pal = match &self.contrast {
            Some(c) => Palette::high_contrast(c),
            None => Palette::with_accent(self.dark, &accent::pick(self.accent_choice, self.system_accent, self.dark)),
        };
        self.theme_dirty = true;
        self.menus_dirty = true;
    }

    /// The colour Windows has as its accent, told at the start and when it changes (used when the settings say `accent=system`).
    pub fn set_system_accent(&mut self, rgb: Option<Rgb>) -> Vec<Action> {
        self.system_accent = rgb;
        if self.accent_choice == AccentChoice::System {
            self.apply_theme();
        }
        vec![Action::Invalidate]
    }

    /// Windows is set to a high-contrast theme (with its colours), or not any more.
    pub fn set_contrast(&mut self, colors: Option<theme::Contrast>) -> Vec<Action> {
        self.contrast = colors;
        self.apply_theme();
        vec![Action::Invalidate]
    }

    /// View > Interface.
    pub(super) fn set_theme(&mut self, choice: ThemeChoice) -> Vec<Action> {
        self.theme_choice = choice;
        self.keep(|s| s.theme = choice);
        self.apply_theme();
        vec![Action::Invalidate]
    }

    /// Windows is set to dark apps or to light ones (told at the start and whenever the setting changes).
    pub fn set_system_dark(&mut self, dark: bool) -> Vec<Action> {
        self.system_dark = dark;
        if self.theme_choice == ThemeChoice::System {
            self.apply_theme();
        }
        vec![Action::Invalidate]
    }

    /// View > Page colours: what is drawn is let go of and drawn again in the new colours (what is on its way is changed when it comes).
    pub(super) fn set_tint(&mut self, tint: Tint) -> Vec<Action> {
        self.keep(|s| s.page_color = tint);
        self.menus_dirty = true;
        if tint == self.tint {
            return Vec::new();
        }
        self.tint = tint;
        self.cache.clear();
        self.cache_tint = tint;
        self.thumbs.forget_pictures();
        self.after_view_change(false)
    }

    /// The colour of the frames round what is picked, over a page: blue, a lighter blue on a dark page.
    pub(super) fn frame_color(&self) -> u32 {
        if self.tint == Tint::Night { 0x66AAFF } else { 0x0066FF }
    }

    /// The opacity of a mark laid over a page (found words, selected text): more of it on a dark page, where the same blue or yellow is dimmer.
    pub(super) fn boost(&self, alpha: u8) -> u8 {
        if self.tint == Tint::Night { alpha.saturating_add(alpha / 2) } else { alpha }
    }

    /// The area round the pages; black in the presentation.
    pub(super) fn page_background(&self) -> u32 {
        if self.present.is_some() { 0 } else { self.pal.background }
    }

    /// The edge of a page; there is none in the presentation.
    pub(super) fn page_border(&self) -> u32 {
        if self.present.is_some() { 0 } else { self.pal.border }
    }

    /// The speed test: the pages two to a row or one, in the colours of the file or others, in the presentation or not.
    pub fn bench_looks(&mut self, spread: Spread, tint: Tint, present: bool) {
        self.default_spread = spread;
        self.tint = tint;
        self.cache_tint = tint;
        self.bench_present = present;
    }

    /// The first page after the row that page `page` is in (a page turn of the speed test).
    pub fn next_row_start(&self, page: usize) -> usize {
        self.doc.as_ref().map_or(page + 1, |d| d.layout.pages_in_row(page).end.max(page + 1))
    }

    /// The number (from 1) of the last page of the row in view: the page itself in a column, the second page of a spread.
    pub(super) fn last_of_row(&self) -> usize {
        self.doc.as_ref().map_or(0, |d| d.layout.pages_in_row(self.current_page()).end)
    }

    /// The pictures of a tab that was not shown were made in the colours of the time: if they have changed since, they are let go of.
    pub(super) fn recheck_tint(&mut self) {
        if self.cache_tint != self.tint {
            self.cache.clear();
            self.cache_tint = self.tint;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::{Rec, open_pages, open_with_catalog, pump};
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn the_layout_is_chosen_remembered_and_comes_back_for_the_next_file() {
        let mut app = open_pages("spread", &[(612, 792, 0); 6], false);
        assert_eq!(app.spread, Spread::Single);
        assert_eq!(app.doc.as_ref().unwrap().layout.pages_in_row(0), 0..1);
        // Two to a row: the first two pages are side by side, and the zoom that fits the width is about half of what it was.
        let before = app.zoom;
        app.event(Event::Command(CMD_LAYOUT_SPREAD));
        let l = &app.doc.as_ref().unwrap().layout;
        assert_eq!((app.spread, l.pages_in_row(1)), (Spread::Pair, 0..2));
        assert!(app.zoom < before * 6 / 10 && app.zoom > before * 4 / 10, "{before} -> {}", app.zoom);
        // Everything fits across: no scrolling sideways, and both pages of the first row are in view.
        assert_eq!(l.max_scroll(app.view_w, app.view_h).0, 0);
        assert!(app.visible().is_some_and(|(a, b)| a == 0 && b >= 1), "{:?}", app.visible());
        assert_eq!(app.default_spread, Spread::Pair);
        // With the first page alone: rows (0), (1, 2).
        app.event(Event::Command(CMD_LAYOUT_COVER));
        let l = &app.doc.as_ref().unwrap().layout;
        assert_eq!((l.pages_in_row(0), l.pages_in_row(1)), (0..1, 1..3));
        // The menu says which, and a click on the check of the one already chosen changes nothing.
        let menus = app.menus();
        let view = menus.iter().find(|m| m.title.contains("View")).unwrap();
        let checked = |id: u32| {
            view.entries.iter().any(|e| match e {
                crate::ui::MenuEntry::Submenu { entries, .. } => entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id: i, checked: true, .. } if *i == id)),
                _ => false,
            })
        };
        assert!(checked(CMD_LAYOUT_COVER) && !checked(CMD_LAYOUT_SPREAD) && !checked(CMD_LAYOUT_SINGLE));
        // The place that is kept says how the pages were.
        assert_eq!(app.current_place().unwrap().layout, Some(Spread::Cover));
        let zoom = app.zoom;
        app.event(Event::Command(CMD_LAYOUT_COVER));
        assert_eq!(app.zoom, zoom);
        // Back to a column: the zoom is what it was.
        app.event(Event::Command(CMD_LAYOUT_SINGLE));
        assert_eq!((app.spread, app.zoom), (Spread::Single, before));
    }

    #[test]
    fn a_file_asks_for_its_layout_a_remembered_place_wins_and_the_last_choice_is_for_the_files_that_ask_nothing() {
        // The word of the file (what the engine told with the pages) is taken when nothing is kept.
        let mut app = open_pages("hint", &[(612, 792, 0); 4], false);
        assert_eq!((app.spread, app.r2l), (Spread::Single, false));
        app.default_spread = Spread::Pair;
        let place = app.current_place().unwrap();
        app.spread = Spread::Single;
        app.apply_place(&Place { layout: Some(Spread::Cover), ..place.clone() });
        assert_eq!(app.spread, Spread::Cover);
        // A place from before the layouts were kept leaves it as it is.
        app.apply_place(&Place { layout: None, ..place });
        assert_eq!(app.spread, Spread::Cover);
        // The settings give the last choice for the files with nothing kept; the setting is read as it was written.
        let mut other = open_pages("default", &[(612, 792, 0); 4], false);
        other.apply_settings(&Settings { layout: Spread::Pair, ..Settings::default() });
        assert_eq!(other.default_spread, Spread::Pair);
    }

    #[test]
    fn the_hint_of_a_file_is_the_layout_and_its_direction_the_way_the_pairs_read() {
        let pages = [(612, 792, 0); 5];
        // A file that asks for two columns, the first page on the right and alone, reading from the right: the cover is on the left of the gutter.
        let app = open_with_catalog("rl", &pages, "/PageLayout /TwoColumnRight /ViewerPreferences << /Direction /R2L >>");
        assert_eq!((app.spread, app.r2l), (Spread::Cover, true));
        let l = &app.doc.as_ref().unwrap().layout;
        assert_eq!((l.arrangement(), l.pages_in_row(0), l.pages_in_row(2)), (Arrangement { spread: Spread::Cover, r2l: true }, 0..1, 1..3));
        assert!(l.left(0, app.view_w, 0) < l.left(1, app.view_w, 0), "the cover is on the left");
        assert!(l.left(1, app.view_w, 0) > l.left(2, app.view_w, 0), "and the pairs read from the right");
        // Two columns with the first page on the left; and a file that asks for nothing takes the last choice.
        assert_eq!(open_with_catalog("l", &pages, "/PageLayout /TwoPageLeft").spread, Spread::Pair);
        assert_eq!(open_with_catalog("one", &pages, "/PageLayout /OneColumn").spread, Spread::Single);
        let mut app = super::super::sidebar::tests::app();
        app.apply_settings(&Settings { layout: Spread::Cover, ..Settings::default() });
        app.event(Event::Size { w: 1000, h: 800 });
        let path = std::env::temp_dir().join(format!("qingpdf-looks-{}.pdf", std::process::id()));
        std::fs::write(&path, super::super::sidebar::tests::pages_pdf(&pages, &[])).expect("writes");
        app.open_path(&path, "");
        pump(&mut app, "the file", |a| a.has_document());
        let _ = std::fs::remove_file(&path);
        assert_eq!(app.spread, Spread::Cover, "nothing asked, nothing kept: the last choice");
    }

    #[test]
    fn the_looks_are_chosen_told_to_the_window_and_kept() {
        let mut app = open_pages("looks", &[(612, 792, 0); 3], false);
        assert_eq!((app.theme_choice, app.tint), (ThemeChoice::System, Tint::Original));
        // Following the system: the window is told what it is now, and again when it changes.
        let told = app.set_system_dark(true);
        assert!(told.iter().any(|a| matches!(a, Action::Invalidate)));
        let actions = app.event(Event::Command(CMD_THEME_DARK));
        assert!(actions.iter().any(|a| matches!(a, Action::Theme(t) if t.dark)));
        assert_eq!(app.pal.background, theme::DARK.background);
        let actions = app.event(Event::Command(CMD_THEME_LIGHT));
        assert!(actions.iter().any(|a| matches!(a, Action::Theme(t) if !t.dark)));
        assert_eq!(app.pal.background, theme::LIGHT.background);
        // Nothing is said when nothing changes.
        let again = app.event(Event::Size { w: 1000, h: 800 });
        assert!(!again.iter().any(|a| matches!(a, Action::Theme(_))));
        app.event(Event::Command(CMD_THEME_SYSTEM));
        assert_eq!(app.pal.background, theme::DARK.background, "the system is dark");
        app.event(Event::SystemDark(false));
        assert_eq!(app.pal.background, theme::LIGHT.background, "and now it is not");
        // A page colour: what was drawn is let go of and drawn again, and the paper under the pages is that colour.
        pump(&mut app, "the pages", |a| a.visible_ready());
        assert!(app.cache.bytes() > 0);
        app.event(Event::Command(CMD_TINT_NIGHT));
        assert_eq!((app.tint, app.cache.bytes()), (Tint::Night, 0));
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.fills.iter().any(|(_, c)| *c == Tint::Night.paper()));
        pump(&mut app, "the pages in night colours", |a| a.visible_ready());
        // The picture that comes is dark where the page is white: the first pixel of the first tile is the night paper.
        let first = app.cache.page(0).find(|e| !e.preview).expect("a tile");
        assert_eq!(&first.bgra[..3], &[0x1C, 0x1C, 0x1C]);
        // Back to the colours of the file.
        app.event(Event::Command(CMD_TINT_ORIGINAL));
        pump(&mut app, "the pages again", |a| a.visible_ready());
        let first = app.cache.page(0).find(|e| !e.preview).expect("a tile");
        assert_eq!(&first.bgra[..3], &[255, 255, 255]);
    }

    #[test]
    fn in_a_spread_the_pointer_the_page_buttons_the_page_box_and_the_status_bar_work_by_the_pages_of_the_row() {
        let mut app = open_pages("spread-use", &[(612, 792, 0); 7], false);
        app.event(Event::Command(CMD_LAYOUT_SPREAD));
        let (vx, vw) = (app.view_x, app.view_w);
        let at = |app: &App, page: usize, dx: i64, dy: i64| {
            let l = &app.doc.as_ref().unwrap().layout;
            (vx + l.left(page, vw, app.scroll_x) + dx, l.top(page) - app.scroll_y + dy)
        };
        // The pointer: the left page of the row, the right one, the place on it, and not the gutter's gap beyond the page.
        let (x, y) = at(&app, 1, 30, 40);
        let (page, px, py) = app.page_at_point(x, y).expect("on page 2");
        assert_eq!((page, px, py), (1, 30.0, 40.0));
        let (x, y) = at(&app, 0, 5, 7);
        assert_eq!(app.page_at_point(x, y).map(|p| (p.0, p.1, p.2)), Some((0, 5.0, 7.0)));
        let (x, y) = at(&app, 0, -3, 7);
        assert!(app.page_at_point(x, y).is_none(), "left of the pages is no page");
        // The page pill and the page box: "1-2 / 7", "1 / 7".
        assert_eq!(pill_page(&app), "1-2 / 7");
        assert_eq!(toolbar_page(&app), "1 / 7");
        // The next page button goes a row on, the one before a row back; at the ends they stop (the last row is page 7 alone).
        let next = |app: &mut App| app.event(Event::Command(CMD_NEXT_PAGE));
        let prev = |app: &mut App| app.event(Event::Command(CMD_PREV_PAGE));
        next(&mut app);
        assert_eq!(app.current_page(), 2);
        next(&mut app);
        next(&mut app);
        assert_eq!(app.current_page(), 6);
        assert_eq!(pill_page(&app), "7 / 7");
        assert!(!button_enabled(&app, CMD_NEXT_PAGE) && button_enabled(&app, CMD_PREV_PAGE));
        next(&mut app);
        assert_eq!(app.current_page(), 6);
        prev(&mut app);
        assert_eq!(app.current_page(), 4);
        // The page box and Ctrl+G go to the page, and the row it is in is shown.
        app.type_page("4");
        assert_eq!((app.current_page(), pill_page(&app)), (2, "3-4 / 7".to_string()));
        assert!(app.scroll_y <= app.doc.as_ref().unwrap().layout.top(3));
        // Both pages of the row are the page in view for the thumbnails.
        app.apply_settings(&Settings { tab: Some(crate::thumbs::Tab::Pages), ..Settings::default() });
        app.event(Event::Size { w: 1000, h: 1100 });
        assert!(app.thumbs_shown());
        let mut rec = Rec::default();
        app.paint(&mut rec);
        // (The tab that is shown is on the same tint.)
        assert_eq!(rec.fills.iter().filter(|(_, c)| *c == app.pal.accent_tint).count(), 2 + 1, "the two pages of the row are marked");
    }

    /// What the page box says now.
    fn toolbar_page(app: &App) -> String {
        crate::cmdbar::page_text(app.current_page(), app.page_count())
    }

    /// What the page pill says now.
    fn pill_page(app: &App) -> String {
        crate::pill::page_label(app.current_page() + 1, app.last_of_row(), app.page_count())
    }

    /// Can the button of the page before (or after) be used?
    fn button_enabled(app: &App, cmd: u32) -> bool {
        app.pill_part_enabled(if cmd == CMD_NEXT_PAGE { crate::pill::Part::Next } else { crate::pill::Part::Prev })
    }

    #[test]
    fn a_spread_is_drawn_with_both_pages_found_words_and_zoom_keep_working_and_the_rotation_too() {
        let mut app = open_pages("spread-draw", &[(612, 792, 0); 4], false);
        app.event(Event::Command(CMD_LAYOUT_SPREAD));
        pump(&mut app, "both pages", |a| a.visible_ready());
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.pictures.len() >= 2, "a picture for each page of the row: {:?}", rec.pictures);
        let l = &app.doc.as_ref().unwrap().layout;
        assert!(rec.pictures.iter().any(|p| p.0.x == (app.view_x + l.left(0, app.view_w, app.scroll_x)) as i32));
        assert!(rec.pictures.iter().any(|p| p.0.x == (app.view_x + l.left(1, app.view_w, app.scroll_x)) as i32));
        // Fit page: the row fits the window, both ways; the zoom is the least of the two fits.
        app.event(Event::Command(CMD_FIT_PAGE));
        let l = &app.doc.as_ref().unwrap().layout;
        assert!(l.size_px(0).1 <= app.view_h && l.content_width() <= app.view_w + 1, "{} {}", l.content_width(), app.view_w);
        // A zoom of its own: the document is wider than the window and the bar scrolls it.
        app.event(Event::Command(CMD_ACTUAL));
        assert!(app.doc.as_ref().unwrap().layout.max_scroll(app.view_w, app.view_h).0 > 0 || app.view_w > 1300);
        app.event(Event::Command(CMD_ROTATE));
        assert_eq!(app.rotation, 90);
        let l = &app.doc.as_ref().unwrap().layout;
        assert_eq!(l.pages_in_row(0), 0..2);
        assert!(l.size_px(0).0 > l.size_px(0).1, "turned");
    }

    #[test]
    fn thumbnails_follow_the_colours_of_the_pages_and_the_menus_say_which() {
        let mut app = open_pages("thumbs-tint", &[(612, 792, 0); 3], true);
        pump(&mut app, "a thumbnail", |a| a.thumbs.cache.get(0).is_some());
        // A thumbnail of a page of the file is white paper at its corner.
        assert_eq!(&app.thumbs.cache.get(0).unwrap().bgra[..3], &[255, 255, 255]);
        app.event(Event::Command(CMD_TINT_SEPIA));
        assert!(app.thumbs.cache.get(0).is_none(), "the pictures of the old colours are let go of");
        pump(&mut app, "a thumbnail in the new colours", |a| a.thumbs.cache.get(0).is_some());
        // Sepia paper is F4ECD8 (the bytes are blue, green, red).
        assert_eq!(&app.thumbs.cache.get(0).unwrap().bgra[..3], &[0xD8, 0xEC, 0xF4]);
        // The choices are ticked in the menus.
        let menus = app.menus();
        let view = menus.iter().find(|m| m.title.contains("View")).unwrap();
        let ticked = |id: u32| {
            view.entries.iter().any(|e| match e {
                crate::ui::MenuEntry::Submenu { entries, .. } => entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id: i, checked: true, .. } if *i == id)),
                _ => false,
            })
        };
        assert!(ticked(CMD_TINT_SEPIA) && !ticked(CMD_TINT_NIGHT) && ticked(CMD_THEME_SYSTEM) && ticked(CMD_LAYOUT_SINGLE));
        // The settings are kept as words (not in the tests: they keep nothing, so what the settings would say is what `Settings` renders).
        let s = Settings { page_color: Tint::Sepia, theme: ThemeChoice::Dark, layout: Spread::Pair, ..Settings::default() };
        assert!(s.render().contains("page_color=sepia\ntheme=dark\nlayout=spread") || (s.render().contains("theme=dark") && s.render().contains("page_color=sepia") && s.render().contains("layout=spread")));
    }
}
