//! How the window looks (2026-10-20, new colours 2026-10-21): the colours of the parts the reader draws itself (the title strip, the sidebar, the page area round the
//! pages, the notes on a page), light or dark, and the choices behind them: View > Interface (follow the system, light, dark) and View > Page
//! colours (as the file has them, night, sepia). Plain data and words; nothing here knows about Windows. The page colours themselves are
//! `qingpdf_core::render::recolor`.

pub use qingpdf_core::render::recolor::Tint;

use crate::accent::{self, Accent};
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

/// The colours of the parts of the window the reader draws, as 0xRRGGBB. Three greys carry the design: the title strip (`strip_bg`: where the window is
/// transparent to Mica), the surface of what is raised from it (the command bar, the tab shown, the cards), and the canvas the pages lie on
/// (`background`); lines between them are `hairline`. The accent colour (青 unless the person chose the system's) is `accent` and its four partners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The canvas round the pages, the edge of a page, the strip between the sidebar and the pages, the hint on an empty window.
    pub background: Rgb,
    pub border: Rgb,
    pub split: Rgb,
    pub hint_fg: Rgb,
    /// The find box laid over the pages and what is raised from the canvas (the page pill, the cards): `status_bg`; and the ink of the words.
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
    /// The title strip, a tab that is not shown (it has no body: only its words and, under the pointer, a fill), the tab shown (the surface), its edge, the
    /// words, the dot of a tab with changes (the accent), the box with the path.
    pub strip_bg: Rgb,
    pub tab_off: Rgb,
    pub tab_on: Rgb,
    pub tab_edge: Rgb,
    pub tab_fg: Rgb,
    pub dirty_fg: Rgb,
    pub tip_bg: Rgb,
    pub tip_fg: Rgb,
    /// The raised surface, the lines round it, the box the words to find are typed in, the fill of a button under the pointer and pressed (laid over a
    /// surface: the ink with these opacities), the shadow under the raised things.
    pub surface: Rgb,
    pub hairline: Rgb,
    pub field_bg: Rgb,
    pub hover_alpha: u8,
    pub press_alpha: u8,
    pub shadow_alpha: u8,
    /// The accent: under the pointer, behind what is chosen, the words on that, the words on the accent itself.
    pub accent_hover: Rgb,
    pub accent_tint: Rgb,
    pub accent_on_tint: Rgb,
    pub on_accent: Rgb,
    /// The little messages over the pages.
    pub toast_bg: Rgb,
    pub toast_fg: Rgb,
}

pub const LIGHT: Palette = Palette {
    background: 0xEBECEC,
    border: 0xC9CFCD,
    split: 0xD6DBD9,
    hint_fg: 0x66706D,
    status_bg: 0xFAFBFB,
    status_fg: 0x1E2422,
    notice_bg: 0xFFF1B8,
    notice_fg: 0x6B4E00,
    list_bg: 0xF4F6F5,
    header_bg: 0xE8ECEB,
    tab_on_bg: 0xFAFBFB,
    accent: accent::QING_LIGHT.base,
    header_edge: 0xD6DBD9,
    selected_border: accent::QING_LIGHT.base,
    selected_bg: accent::QING_LIGHT.tint,
    current_frame: 0x7CC6AF,
    thumb_border: 0xB4BCB9,
    placeholder: 0xF7F8F8,
    damaged: 0xD3D8D6,
    number_fg: 0x3A4240,
    selected_fg: accent::QING_LIGHT.on_tint,
    disabled_fg: 0xA3ABA8,
    dim_fg: 0x5E6865,
    bar_color: 0xB4BCB9,
    strip_bg: 0xE8ECEB,
    tab_off: 0xE8ECEB,
    tab_on: 0xFAFBFB,
    tab_edge: 0xD6DBD9,
    tab_fg: 0x1E2422,
    dirty_fg: accent::QING_LIGHT.base,
    tip_bg: 0xFAFBFB,
    tip_fg: 0x1E2422,
    surface: 0xFAFBFB,
    hairline: 0xD6DBD9,
    field_bg: 0xEFF2F1,
    hover_alpha: 18,
    press_alpha: 32,
    shadow_alpha: 26,
    accent_hover: accent::QING_LIGHT.hover,
    accent_tint: accent::QING_LIGHT.tint,
    accent_on_tint: accent::QING_LIGHT.on_tint,
    on_accent: accent::QING_LIGHT.on_base,
    toast_bg: 0x2A2F2D,
    toast_fg: 0xF4F6F5,
};

pub const DARK: Palette = Palette {
    background: 0x1B1B1B,
    border: 0x3F4543,
    split: 0x363B39,
    hint_fg: 0x9AA4A1,
    status_bg: 0x2A2E2D,
    status_fg: 0xE6EAE8,
    notice_bg: 0x4A3F1A,
    notice_fg: 0xF0D890,
    list_bg: 0x232726,
    header_bg: 0x1F2221,
    tab_on_bg: 0x2A2E2D,
    accent: accent::QING_DARK.base,
    header_edge: 0x363B39,
    selected_border: accent::QING_DARK.base,
    selected_bg: accent::QING_DARK.tint,
    current_frame: 0x478771,
    thumb_border: 0x56605D,
    placeholder: 0x2E3231,
    damaged: 0x444B49,
    number_fg: 0xD2D8D6,
    selected_fg: accent::QING_DARK.on_tint,
    disabled_fg: 0x6A7471,
    dim_fg: 0x9CA6A3,
    bar_color: 0x56605D,
    strip_bg: 0x1F2221,
    tab_off: 0x1F2221,
    tab_on: 0x2A2E2D,
    tab_edge: 0x363B39,
    tab_fg: 0xE6EAE8,
    dirty_fg: accent::QING_DARK.base,
    tip_bg: 0x2A2E2D,
    tip_fg: 0xE6EAE8,
    surface: 0x2A2E2D,
    hairline: 0x3B4140,
    field_bg: 0x222625,
    hover_alpha: 22,
    press_alpha: 40,
    shadow_alpha: 70,
    accent_hover: accent::QING_DARK.hover,
    accent_tint: accent::QING_DARK.tint,
    accent_on_tint: accent::QING_DARK.on_tint,
    on_accent: accent::QING_DARK.on_base,
    toast_bg: 0x3A403E,
    toast_fg: 0xF2F5F4,
};

/// The colours Windows gives when it is set to a high-contrast theme (`GetSysColor`): the ground of a window, its words, the words of what is
/// off, the colour of what is chosen and the words on it, the face of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contrast {
    pub window: Rgb,
    pub text: Rgb,
    pub gray: Rgb,
    pub highlight: Rgb,
    pub highlight_text: Rgb,
    pub face: Rgb,
}

impl Palette {
    pub fn of(dark: bool) -> &'static Palette {
        if dark { &DARK } else { &LIGHT }
    }

    /// The palette of a light or dark window with this accent in place of 青 (the parts the accent decides are put over the others).
    pub fn with_accent(dark: bool, a: &Accent) -> Palette {
        let base = *Palette::of(dark);
        Palette {
            accent: a.base,
            accent_hover: a.hover,
            accent_tint: a.tint,
            accent_on_tint: a.on_tint,
            on_accent: a.on_base,
            selected_border: a.base,
            selected_bg: a.tint,
            selected_fg: a.on_tint,
            dirty_fg: a.base,
            current_frame: accent::mix(a.base, base.surface, 110),
            ..base
        }
    }

    /// The palette for a high-contrast theme: the colours of the system, everywhere, with a line round each part.
    pub fn high_contrast(c: &Contrast) -> Palette {
        Palette {
            background: c.window,
            border: c.text,
            split: c.text,
            hint_fg: c.text,
            status_bg: c.window,
            status_fg: c.text,
            notice_bg: c.window,
            notice_fg: c.text,
            list_bg: c.window,
            header_bg: c.face,
            tab_on_bg: c.window,
            accent: c.highlight,
            header_edge: c.text,
            selected_border: c.highlight,
            selected_bg: c.highlight,
            current_frame: c.highlight,
            thumb_border: c.text,
            placeholder: c.window,
            damaged: c.gray,
            number_fg: c.text,
            selected_fg: c.highlight_text,
            disabled_fg: c.gray,
            dim_fg: c.text,
            bar_color: c.text,
            strip_bg: c.face,
            tab_off: c.face,
            tab_on: c.window,
            tab_edge: c.text,
            tab_fg: c.text,
            dirty_fg: c.highlight,
            tip_bg: c.window,
            tip_fg: c.text,
            surface: c.window,
            hairline: c.text,
            field_bg: c.window,
            hover_alpha: 40,
            press_alpha: 70,
            shadow_alpha: 0,
            accent_hover: c.highlight,
            accent_tint: c.highlight,
            accent_on_tint: c.highlight_text,
            on_accent: c.highlight_text,
            toast_bg: c.window,
            toast_fg: c.text,
        }
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

    #[test]
    fn the_design_tokens_have_the_contrast_they_need() {
        for dark in [false, true] {
            let p = Palette::of(dark);
            // The ink on the surface, the dimmer ink and the words on the tint of what is chosen.
            assert!(accent::contrast(p.status_fg, p.surface) >= 9.0, "{dark}");
            assert!(accent::contrast(p.dim_fg, p.surface) >= 4.5, "{dark}");
            assert!(accent::contrast(p.accent_on_tint, p.accent_tint) >= 4.5, "{dark}");
            assert!(accent::contrast(p.toast_fg, p.toast_bg) >= 9.0, "{dark}");
            // The raised surface stands out from the strip and from the canvas, a little.
            assert_ne!(p.surface, p.strip_bg);
            assert_ne!(p.surface, p.background);
            assert!(accent::contrast(p.hairline, p.surface) > 1.1);
            // The accent shows on the surface (3 to 1) and the tab shown is the surface.
            assert!(accent::contrast(p.accent, p.surface) >= 3.0, "{dark}");
            assert_eq!((p.tab_on, p.status_bg), (p.surface, p.surface));
        }
        // The canvas is the grey that was asked for.
        assert_eq!((LIGHT.background, DARK.background), (0xEBECEC, 0x1B1B1B));
    }

    #[test]
    fn the_accent_is_qing_and_a_system_accent_replaces_every_colour_it_decides() {
        assert_eq!((LIGHT.accent, DARK.accent), (0x1D9E75, 0x5DCAA5));
        assert_eq!(Palette::with_accent(false, &accent::QING_LIGHT), LIGHT);
        assert_eq!(Palette::with_accent(true, &accent::QING_DARK), DARK);
        let blue = accent::from_system(0x0078D4, false);
        let p = Palette::with_accent(false, &blue);
        assert_eq!((p.accent, p.accent_hover, p.accent_tint, p.selected_bg, p.dirty_fg, p.selected_border), (blue.base, blue.hover, blue.tint, blue.tint, blue.base, blue.base));
        // What the accent does not decide is as it was.
        assert_eq!((p.surface, p.background, p.strip_bg), (LIGHT.surface, LIGHT.background, LIGHT.strip_bg));
    }

    #[test]
    fn a_high_contrast_window_has_the_colours_of_the_system_and_nothing_else() {
        let c = Contrast { window: 0x000000, text: 0xFFFFFF, gray: 0x3FF23F, highlight: 0x1AEBFF, highlight_text: 0x000000, face: 0x000000 };
        let p = Palette::high_contrast(&c);
        assert_eq!((p.background, p.surface, p.strip_bg, p.status_fg, p.accent), (0x000000, 0x000000, 0x000000, 0xFFFFFF, 0x1AEBFF));
        assert_eq!((p.disabled_fg, p.selected_fg, p.selected_bg), (0x3FF23F, 0x000000, 0x1AEBFF));
        // Lines are drawn in the colour of the words, and there is no shadow.
        assert_eq!((p.hairline, p.border, p.shadow_alpha), (0xFFFFFF, 0xFFFFFF, 0));
        let light = Palette::high_contrast(&Contrast { window: 0xFFFFFF, text: 0x000000, gray: 0x600000, highlight: 0x800080, highlight_text: 0xFFFFFF, face: 0xFFFFFF });
        assert_eq!((light.background, light.status_fg), (0xFFFFFF, 0x000000));
    }
}
