//! How the window looks (2026-10-20): the colours of the parts the reader draws itself (the strip of tabs, the sidebar, the page area round the
//! pages, the notes on a page), light or dark, and the choices behind them: View > Interface (follow the system, light, dark) and View > Page
//! colours (as the file has them, night, sepia). Plain data and words; nothing here knows about Windows. The page colours themselves are
//! `qingpdf_core::render::recolor`.

pub use qingpdf_core::render::recolor::Tint;

use crate::ui::Rgb;

/// What the person chose in View > Interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// The word kept in the settings.
    pub fn word(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Option<ThemeChoice> {
        [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark].into_iter().find(|c| c.word() == s.trim().to_ascii_lowercase())
    }

    /// Is the window dark, given whether Windows is set to dark apps?
    pub fn dark(self, system_dark: bool) -> bool {
        match self {
            ThemeChoice::System => system_dark,
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
        }
    }
}

/// The word kept in the settings for a page colour.
pub fn tint_word(tint: Tint) -> &'static str {
    match tint {
        Tint::Original => "original",
        Tint::Night => "night",
        Tint::Sepia => "sepia",
    }
}

pub fn parse_tint(s: &str) -> Option<Tint> {
    [Tint::Original, Tint::Night, Tint::Sepia].into_iter().find(|&t| tint_word(t) == s.trim().to_ascii_lowercase())
}

/// The colours of the parts of the window the reader draws, as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The area round the pages, the edge of a page, the strip between the sidebar and the pages, the hint on an empty window.
    pub background: Rgb,
    pub border: Rgb,
    pub split: Rgb,
    pub hint_fg: Rgb,
    /// The find box laid over the pages, and the status bar (which Windows draws, in the same colours when the window is dark).
    pub status_bg: Rgb,
    pub status_fg: Rgb,
    /// The strip on a page that could not be drawn whole.
    pub notice_bg: Rgb,
    pub notice_fg: Rgb,
    /// The sidebar: its list, its header with the tabs, the tab shown, the line under it, the lines between.
    pub list_bg: Rgb,
    pub header_bg: Rgb,
    pub tab_on_bg: Rgb,
    pub accent: Rgb,
    pub header_edge: Rgb,
    /// A row chosen, the page in view when others are chosen, a thumbnail's edge and its stand-ins, the numbers, the bar.
    pub selected_border: Rgb,
    pub selected_bg: Rgb,
    pub current_frame: Rgb,
    pub thumb_border: Rgb,
    pub placeholder: Rgb,
    pub damaged: Rgb,
    pub number_fg: Rgb,
    pub selected_fg: Rgb,
    pub disabled_fg: Rgb,
    pub dim_fg: Rgb,
    pub bar_color: Rgb,
    /// The strip of tabs: the strip, a tab, the tab shown, its edge, the words, a tab with changes, the box with the path.
    pub strip_bg: Rgb,
    pub tab_off: Rgb,
    pub tab_on: Rgb,
    pub tab_edge: Rgb,
    pub tab_fg: Rgb,
    pub dirty_fg: Rgb,
    pub tip_bg: Rgb,
    pub tip_fg: Rgb,
}

pub const LIGHT: Palette = Palette {
    background: 0xC8C8C8,
    border: 0x8C8C8C,
    split: 0xA0A0A0,
    hint_fg: 0x505050,
    status_bg: 0xF0F0F0,
    status_fg: 0x202020,
    notice_bg: 0xFFF1B8,
    notice_fg: 0x6B4E00,
    list_bg: 0xEEEEEE,
    header_bg: 0xE0E0E0,
    tab_on_bg: 0xFAFAFA,
    accent: 0x2B7CFF,
    header_edge: 0xB4B4B4,
    selected_border: 0x3390FF,
    selected_bg: 0xD3E3FB,
    current_frame: 0x8FB8F0,
    thumb_border: 0x8C8C8C,
    placeholder: 0xF7F7F7,
    damaged: 0xCDCDCD,
    number_fg: 0x404040,
    selected_fg: 0x0B4FB3,
    disabled_fg: 0x9A9A9A,
    dim_fg: 0x707070,
    bar_color: 0xB4B4B4,
    strip_bg: 0xD4D4D4,
    tab_off: 0xE2E2E2,
    tab_on: 0xF8F8F8,
    tab_edge: 0xA8A8A8,
    tab_fg: 0x303030,
    dirty_fg: 0xD0560B,
    tip_bg: 0xFFFFE1,
    tip_fg: 0x202020,
};

pub const DARK: Palette = Palette {
    background: 0x2B2B2B,
    border: 0x5A5A5A,
    split: 0x555555,
    hint_fg: 0xAAAAAA,
    status_bg: 0x2D2D2D,
    status_fg: 0xE2E2E2,
    notice_bg: 0x4A3F1A,
    notice_fg: 0xF0D890,
    list_bg: 0x252525,
    header_bg: 0x333333,
    tab_on_bg: 0x252525,
    accent: 0x4C9AFF,
    header_edge: 0x4D4D4D,
    selected_border: 0x4C9AFF,
    selected_bg: 0x2A3F5F,
    current_frame: 0x4F7099,
    thumb_border: 0x666666,
    placeholder: 0x2E2E2E,
    damaged: 0x444444,
    number_fg: 0xD0D0D0,
    selected_fg: 0x8FC0FF,
    disabled_fg: 0x6E6E6E,
    dim_fg: 0x9A9A9A,
    bar_color: 0x5A5A5A,
    strip_bg: 0x1F1F1F,
    tab_off: 0x2B2B2B,
    tab_on: 0x3A3A3A,
    tab_edge: 0x4A4A4A,
    tab_fg: 0xE0E0E0,
    dirty_fg: 0xFF8A4D,
    tip_bg: 0x3A3A3A,
    tip_fg: 0xE8E8E8,
};

impl Palette {
    pub fn of(dark: bool) -> &'static Palette {
        if dark { &DARK } else { &LIGHT }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_come_back_from_their_words_and_the_system_decides_only_when_asked() {
        for c in [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark] {
            assert_eq!(ThemeChoice::parse(c.word()), Some(c));
        }
        assert_eq!(ThemeChoice::parse(" DARK "), Some(ThemeChoice::Dark));
        assert_eq!(ThemeChoice::parse("black"), None);
        for t in [Tint::Original, Tint::Night, Tint::Sepia] {
            assert_eq!(parse_tint(tint_word(t)), Some(t));
        }
        assert_eq!(parse_tint("Sepia"), Some(Tint::Sepia));
        assert_eq!(parse_tint("pink"), None);
        assert_eq!((ThemeChoice::System.dark(true), ThemeChoice::System.dark(false)), (true, false));
        assert_eq!((ThemeChoice::Light.dark(true), ThemeChoice::Dark.dark(false)), (false, true));
    }

    /// The brightness of 0xRRGGBB, 0 to 255.
    fn luma(c: Rgb) -> i32 {
        (77 * ((c >> 16) & 255) as i32 + 150 * ((c >> 8) & 255) as i32 + 29 * (c & 255) as i32) >> 8
    }

    #[test]
    fn the_dark_colours_are_dark_where_the_light_ones_are_light_and_the_words_can_be_read() {
        let (l, d) = (Palette::of(false), Palette::of(true));
        for (name, light, dark) in [
            ("background", l.background, d.background),
            ("status", l.status_bg, d.status_bg),
            ("list", l.list_bg, d.list_bg),
            ("header", l.header_bg, d.header_bg),
            ("strip", l.strip_bg, d.strip_bg),
            ("tab", l.tab_on, d.tab_on),
            ("placeholder", l.placeholder, d.placeholder),
        ] {
            assert!(luma(light) > 128 && luma(dark) < 80, "{name}: {light:06X} {dark:06X}");
        }
        // Words are readable on what they are drawn on: a difference of at least 90 in brightness, in either palette.
        for p in [l, d] {
            for (fg, bg) in [(p.status_fg, p.status_bg), (p.number_fg, p.list_bg), (p.number_fg, p.header_bg), (p.tab_fg, p.tab_on), (p.tab_fg, p.tab_off), (p.tab_fg, p.strip_bg), (p.notice_fg, p.notice_bg), (p.tip_fg, p.tip_bg), (p.selected_fg, p.header_bg)] {
                assert!((luma(fg) - luma(bg)).abs() >= 90, "{fg:06X} on {bg:06X}");
            }
            assert!((luma(p.dim_fg) - luma(p.list_bg)).abs() >= 40 && (luma(p.hint_fg) - luma(p.background)).abs() >= 40);
        }
        // The page's edge shows against the area round it, the chosen row against the list.
        assert!((luma(d.border) - luma(d.background)).abs() >= 20 && (luma(d.selected_bg) - luma(d.list_bg)).abs() >= 10);
        assert_ne!(LIGHT, DARK);
    }
}
