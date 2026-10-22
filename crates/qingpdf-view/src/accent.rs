//! The accent colour of the window (2026-10-21): 青 (teal), or, when the settings say `accent=system`, the colour the person chose in Windows. From
//! one colour the window needs five: the colour itself (the mark of the active tab, the filled button, the tool in use), the same under the pointer
//! or pressed, a tint to lay behind what is chosen, the words on that tint, and the words on the colour itself. Plain arithmetic; the system's colour
//! is read by `win32.rs`.

use crate::ui::Rgb;

/// What the person chose in the settings (`accent=qing` or `accent=system`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccentChoice {
    #[default]
    Qing,
    System,
}

impl AccentChoice {
    pub fn word(self) -> &'static str {
        match self {
            AccentChoice::Qing => "qing",
            AccentChoice::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<AccentChoice> {
        [AccentChoice::Qing, AccentChoice::System].into_iter().find(|c| c.word() == s.trim().to_ascii_lowercase())
    }
}

/// The five colours, as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accent {
    pub base: Rgb,
    /// Under the pointer and pressed.
    pub hover: Rgb,
    /// Behind what is chosen, and the words on it.
    pub tint: Rgb,
    pub on_tint: Rgb,
    /// The words on `base`.
    pub on_base: Rgb,
}

/// 青 in a light window and in a dark one.
pub const QING_LIGHT: Accent = Accent { base: 0x1D9E75, hover: 0x0F6E56, tint: 0xE1F5EE, on_tint: 0x0F6E56, on_base: 0xFFFFFF };
pub const QING_DARK: Accent = Accent { base: 0x5DCAA5, hover: 0x7BD7B7, tint: 0x085041, on_tint: 0x9FE1CB, on_base: 0x07261E };

/// The accent a window gets: 青, or `system` (the colour Windows is set to; `None` when it could not be read) made to suit the window.
pub fn pick(choice: AccentChoice, system: Option<Rgb>, dark: bool) -> Accent {
    match (choice, system) {
        (AccentChoice::System, Some(rgb)) => from_system(rgb, dark),
        _ => {
            if dark {
                QING_DARK
            } else {
                QING_LIGHT
            }
        }
    }
}

fn channels(c: Rgb) -> [u32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255]
}

fn pack(c: [u32; 3]) -> Rgb {
    (c[0].min(255) << 16) | (c[1].min(255) << 8) | c[2].min(255)
}

/// `a` with `b` laid over it, `part` of 256 of `b`.
pub fn mix(a: Rgb, b: Rgb, part: u32) -> Rgb {
    let part = part.min(256);
    let m = |from: u32, to: u32| (from * (256 - part) + to * part + 128) >> 8;
    let ([a0, a1, a2], [b0, b1, b2]) = (channels(a), channels(b));
    pack([m(a0, b0), m(a1, b1), m(a2, b2)])
}

/// The relative luminance of a colour (WCAG), 0.0 to 1.0.
pub fn luminance(c: Rgb) -> f32 {
    let lin = |v: u32| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    let [r, g, b] = channels(c).map(lin);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The contrast between two colours (WCAG), 1.0 to 21.0.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The colour Windows is set to, made to suit a light or a dark window: a colour that would be too pale to see on a light window is darkened, one too
/// dark on a dark window is lightened, until it shows (3 to 1, the least for a part of an interface); the others come from it.
pub fn from_system(rgb: Rgb, dark: bool) -> Accent {
    let (ground, toward) = if dark { (0x2B2B2B, 0xFFFFFF) } else { (0xFAFAFA, 0x000000) };
    let mut base = rgb & 0xFF_FFFF;
    let mut steps = 0;
    while contrast(base, ground) < 3.0 && steps < 16 {
        base = mix(base, toward, 24);
        steps += 1;
    }
    let (hover, tint, on_tint) = if dark {
        (mix(base, 0xFFFFFF, 40), mix(base, 0x000000, 175), mix(base, 0xFFFFFF, 140))
    } else {
        (mix(base, 0x000000, 56), mix(base, 0xFFFFFF, 226), mix(base, 0x000000, 90))
    };
    // The words on the colour: white if they show at 3 to 1 or better (they are large and bold), else a near black.
    let on_base = if contrast(0xFFFFFF, base) >= 3.0 { 0xFFFFFF } else { 0x101010 };
    Accent { base, hover, tint, on_tint, on_base }
}

/// The colour from what Windows keeps as its accent (`AccentColor` of the registry: 0xAABBGGRR) or colourization colour (`0xAARRGGBB`).
pub fn rgb_of_abgr(abgr: u32) -> Rgb {
    ((abgr & 0xFF) << 16) | (abgr & 0xFF00) | ((abgr >> 16) & 0xFF)
}

pub fn rgb_of_argb(argb: u32) -> Rgb {
    argb & 0xFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_comes_back_from_its_word_and_qing_is_the_usual() {
        for c in [AccentChoice::Qing, AccentChoice::System] {
            assert_eq!(AccentChoice::parse(c.word()), Some(c));
        }
        assert_eq!(AccentChoice::parse(" System "), Some(AccentChoice::System));
        assert_eq!(AccentChoice::parse("pink"), None);
        assert_eq!(AccentChoice::default(), AccentChoice::Qing);
    }

    #[test]
    fn qing_is_the_colours_that_were_chosen() {
        assert_eq!((QING_LIGHT.base, QING_LIGHT.hover, QING_LIGHT.tint, QING_LIGHT.on_tint), (0x1D9E75, 0x0F6E56, 0xE1F5EE, 0x0F6E56));
        assert_eq!((QING_DARK.base, QING_DARK.tint, QING_DARK.on_tint), (0x5DCAA5, 0x085041, 0x9FE1CB));
        // Qing is what a window gets unless `system` is chosen and read; `system` that could not be read is Qing too.
        assert_eq!(pick(AccentChoice::Qing, Some(0xFF0000), false), QING_LIGHT);
        assert_eq!(pick(AccentChoice::System, None, true), QING_DARK);
        assert_ne!(pick(AccentChoice::System, Some(0xFF0000), false), QING_LIGHT);
    }

    #[test]
    fn contrast_is_the_wcag_ratio() {
        assert!((contrast(0xFFFFFF, 0x000000) - 21.0).abs() < 0.01);
        assert!((contrast(0x777777, 0x777777) - 1.0).abs() < 0.001);
        // The words of the dark window's filled button and of the light one's are readable on their colours.
        assert!(contrast(QING_DARK.on_base, QING_DARK.base) >= 7.0);
        assert!(contrast(QING_LIGHT.on_base, QING_LIGHT.base) >= 3.0);
        assert!(contrast(QING_LIGHT.on_tint, QING_LIGHT.tint) >= 4.5 && contrast(QING_DARK.on_tint, QING_DARK.tint) >= 4.5);
    }

    #[test]
    fn a_system_colour_is_made_to_show_on_the_window_it_is_in_and_its_words_can_be_read() {
        for dark in [false, true] {
            let ground = if dark { 0x2B2B2B } else { 0xFAFAFA };
            for system in [0x0078D4, 0xFFB900, 0xFFFF00, 0x000000, 0xFFFFFF, 0xE81123, 0x10893E, 0x767676] {
                let a = from_system(system, dark);
                assert!(contrast(a.base, ground) >= 3.0, "{system:06X} dark={dark}: {:06X}", a.base);
                assert!(contrast(a.on_base, a.base) >= 3.0, "{system:06X} dark={dark}: words {:06X} on {:06X}", a.on_base, a.base);
                assert!(contrast(a.on_tint, a.tint) >= 4.5, "{system:06X} dark={dark}: {:06X} on {:06X}", a.on_tint, a.tint);
                // The tint is a quiet colour: nearer the window than the colour itself is.
                assert!(contrast(a.tint, ground) < contrast(a.base, ground));
            }
        }
    }

    #[test]
    fn a_colour_that_already_shows_is_kept_and_the_others_come_from_it() {
        let a = from_system(0x0078D4, false);
        assert_eq!(a.base, 0x0078D4);
        // The colour under the pointer is darker on a light window, lighter on a dark one.
        assert!(luminance(a.hover) < luminance(a.base));
        let d = from_system(0x4CC2FF, true);
        assert_eq!(d.base, 0x4CC2FF);
        assert!(luminance(d.hover) > luminance(d.base));
        // A pale yellow on a light window is darkened until it shows.
        assert!(luminance(from_system(0xFFFF00, false).base) < luminance(0xFFFF00));
    }

    #[test]
    fn what_windows_keeps_is_read_in_its_byte_orders() {
        assert_eq!(rgb_of_abgr(0xFF_D4_78_00), 0x0078D4);
        assert_eq!(rgb_of_argb(0xFF_00_78_D4), 0x0078D4);
    }

    #[test]
    fn mixing_goes_from_one_colour_to_the_other() {
        assert_eq!(mix(0x102030, 0xF0E0D0, 0), 0x102030);
        assert_eq!(mix(0x102030, 0xF0E0D0, 256), 0xF0E0D0);
        assert_eq!(mix(0x000000, 0xFFFFFF, 128), 0x808080);
    }
}
