//! What the reader keeps between runs, in one small UTF-8 text file, `%APPDATA%\qingpdf\author.txt` (the name it has had since the
//! name put on the annotations, 4a, was kept there). The first line is that name, as it always was, so that an older build still reads
//! it; the lines after it are `key=value` settings: `lang` (`system`, `zh` or `en`), `toolbar` (`1` or `0`), `sidebar` (the width of
//! the sidebar, in pixels at 96 dpi), `tab` (`bookmarks`, `pages` or `annots`: the tab of the sidebar last used), `window` (`x,y,w,h,maximized`, see [`Placement`]),
//! and (2026-10-17) `single_window` and `remember_place` (`0` turns off handing a file to the copy of the program that is running, and opening a
//! file where it was left; both are on unless they are `0`), and (2026-10-20) `theme` (`system`, `light` or `dark`: the look of the window), `page_color`
//! (`original`, `night` or `sepia`) and `layout` (`single`, `spread` or `cover`: how the pages of a file nothing is kept about are laid out); those three are
//! written only when they are not the first of their words. Nothing is sent anywhere. The file is
//! not trusted (another program may have written it): a name that is too long or has control characters in it is not used, a setting
//! that is not understood or is out of range is ignored.

use std::io::Read;
use std::path::PathBuf;

use qingpdf_core::view::MAX_AUTHOR_CHARS;

use crate::i18n::LangChoice;
use crate::layout::Spread;
use crate::placement::Placement;
use crate::theme::{self, ThemeChoice, Tint};
use crate::thumbs::Tab;

/// Most bytes of the file that are read.
const MAX_FILE_BYTES: u64 = 4096;
/// The widths of the bookmarks that are kept, at 96 dpi.
pub const SIDEBAR_RANGE: std::ops::RangeInclusive<i32> = 120..=800;

/// The folder the reader keeps its files in: `%APPDATA%\qingpdf` (none if there is no `APPDATA`). In the unit tests it is a folder of the
/// temporary files instead (one for each run of the tests), so that no test reads or changes the person's own files.
#[cfg(not(test))]
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").filter(|b| !b.is_empty())?;
    Some(PathBuf::from(base).join("qingpdf"))
}

#[cfg(test)]
pub fn data_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir().join(format!("qingpdf-view-test-{}", std::process::id())))
}

/// Where the settings are kept (none if there is no `APPDATA`).
pub fn file_path() -> Option<PathBuf> {
    Some(data_dir()?.join("author.txt"))
}

/// A name the annotations may carry: not empty, at most [`MAX_AUTHOR_CHARS`] characters, no control characters. Whitespace at the
/// ends is dropped.
pub fn clean(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= MAX_AUTHOR_CHARS && !name.chars().any(char::is_control)).then(|| name.to_string())
}

/// Everything kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub author: Option<String>,
    pub lang: LangChoice,
    /// The toolbar is shown.
    pub toolbar: bool,
    /// The width of the bookmarks beside the pages, at 96 dpi (none: the usual).
    pub sidebar: Option<i32>,
    /// The tab of the sidebar last used (none: the bookmarks).
    pub tab: Option<Tab>,
    pub window: Option<Placement>,
    /// A file opened from outside goes to the copy of the program that is running (else a window of its own), and a file opens where it was left.
    pub single_window: bool,
    pub remember_place: bool,
    /// How the window looks (View > Interface), the colours of the pages (View > Page colours) and how the pages of a file with nothing kept about it are laid
    /// out (View > Page layout: the last one chosen).
    pub theme: ThemeChoice,
    pub page_color: Tint,
    pub layout: Spread,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            author: None,
            lang: LangChoice::System,
            toolbar: true,
            sidebar: None,
            tab: None,
            window: None,
            single_window: true,
            remember_place: true,
            theme: ThemeChoice::System,
            page_color: Tint::Original,
            layout: Spread::Single,
        }
    }
}

impl Settings {
    /// Read the text of the file.
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        let mut lines = text.lines();
        s.author = clean(lines.next().unwrap_or(""));
        for line in lines {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "lang" => s.lang = LangChoice::parse(value).unwrap_or_default(),
                "toolbar" => s.toolbar = value.trim() != "0",
                "sidebar" => s.sidebar = value.trim().parse::<i32>().ok().filter(|w| SIDEBAR_RANGE.contains(w)),
                "tab" => s.tab = Tab::parse(value),
                "window" => s.window = Placement::parse(value),
                "single_window" => s.single_window = value.trim() != "0",
                "remember_place" => s.remember_place = value.trim() != "0",
                "theme" => s.theme = ThemeChoice::parse(value).unwrap_or_default(),
                "page_color" => s.page_color = theme::parse_tint(value).unwrap_or_default(),
                "layout" => s.layout = Spread::parse(value).unwrap_or_default(),
                _ => {}
            }
        }
        s
    }

    /// The text of the file.
    pub fn render(&self) -> String {
        let mut out = format!("{}\nlang={}\ntoolbar={}\n", self.author.as_deref().unwrap_or(""), self.lang.as_str(), i32::from(self.toolbar));
        if let Some(w) = self.sidebar {
            out.push_str(&format!("sidebar={w}\n"));
        }
        if let Some(t) = self.tab {
            out.push_str(&format!("tab={}\n", t.as_str()));
        }
        if let Some(w) = self.window {
            out.push_str(&format!("window={}\n", w.to_text()));
        }
        if !self.single_window {
            out.push_str("single_window=0\n");
        }
        if !self.remember_place {
            out.push_str("remember_place=0\n");
        }
        // (The look and the layout are written when they are not the usual.)
        for (key, value, usual) in [("theme", self.theme.word(), "system"), ("page_color", theme::tint_word(self.page_color), "original"), ("layout", self.layout.word(), "single")] {
            if value != usual {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
        out
    }
}

/// The settings in the file (the usual ones when there is none, or it cannot be read).
pub fn load_all() -> Settings {
    let mut text = String::new();
    let read = file_path().and_then(|p| std::fs::File::open(p).ok()).and_then(|f| f.take(MAX_FILE_BYTES).read_to_string(&mut text).ok());
    if read.is_some() { Settings::parse(&text) } else { Settings::default() }
}

/// Change the settings in the file: it is read, `change` is applied and it is written again (what cannot be written is not an error:
/// the change is then only for this run).
pub fn update(change: impl FnOnce(&mut Settings)) {
    let Some(path) = file_path() else { return };
    let mut s = load_all();
    change(&mut s);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, s.render());
}

/// The saved name, if there is one that [`clean`] accepts.
pub fn load() -> Option<String> {
    load_all().author
}

/// Keep `name` for the next time.
pub fn save(name: &str) {
    update(|s| s.author = clean(name));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_trimmed_and_held_to_the_limits() {
        assert_eq!(clean("  Vincent  ").as_deref(), Some("Vincent"));
        assert_eq!(clean("张三").as_deref(), Some("张三"));
        assert_eq!(clean(""), None);
        assert_eq!(clean("   "), None);
        assert_eq!(clean("a\u{7}b"), None);
        assert_eq!(clean(&"x".repeat(MAX_AUTHOR_CHARS)).map(|s| s.len()), Some(MAX_AUTHOR_CHARS));
        assert_eq!(clean(&"x".repeat(MAX_AUTHOR_CHARS + 1)), None);
    }

    #[test]
    fn the_tests_keep_their_files_out_of_the_persons_folder() {
        let dir = data_dir().unwrap();
        assert!(dir.starts_with(std::env::temp_dir()), "{dir:?}");
        assert!(file_path().unwrap().starts_with(&dir));
        assert!(crate::recent::file_path().unwrap().starts_with(&dir));
    }

    #[test]
    fn a_file_of_only_a_name_is_what_the_older_builds_wrote() {
        let s = Settings::parse("Vincent\n");
        assert_eq!(s, Settings { author: Some("Vincent".to_string()), ..Settings::default() });
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(Settings::parse("\n\n").author, None);
    }

    #[test]
    fn what_is_written_is_read_back() {
        let s = Settings {
            author: Some("张三".to_string()),
            lang: LangChoice::Zh,
            toolbar: false,
            sidebar: Some(310),
            tab: Some(Tab::Pages),
            window: Some(Placement { x: -1500, y: 20, w: 1100, h: 760, maximized: true }),
            single_window: false,
            remember_place: false,
            theme: ThemeChoice::Dark,
            page_color: Tint::Sepia,
            layout: Spread::Cover,
        };
        let text = s.render();
        assert_eq!(text.lines().next(), Some("张三"));
        assert_eq!(Settings::parse(&text), s);
        // Without a name the first line is empty, and the settings still follow it.
        let nameless = Settings { author: None, ..s.clone() };
        assert_eq!(Settings::parse(&nameless.render()), nameless);
        // The name of a person who is called "a=b" is the name, not a setting.
        let odd = Settings { author: Some("a=b".to_string()), ..Settings::default() };
        assert_eq!(Settings::parse(&odd.render()).author.as_deref(), Some("a=b"));
    }

    #[test]
    fn a_setting_that_is_not_understood_or_out_of_range_is_ignored() {
        let s = Settings::parse("N\nlang=fr\ntoolbar=0\nsidebar=5\nwindow=1,2,3\nunknown=1\nno equals sign\nsidebar=99999\n");
        assert_eq!(s.lang, LangChoice::System);
        assert!(!s.toolbar);
        assert_eq!(s.sidebar, None);
        assert_eq!(s.window, None);
        assert_eq!(Settings::parse("N\nsidebar=200\nsidebar=oops\n").sidebar, None);
        assert_eq!(Settings::parse("N\nsidebar=200\n").sidebar, Some(200));
        assert_eq!(Settings::parse("N\ntab=pages\n").tab, Some(Tab::Pages));
        // Both are on unless they say 0 (anything else, or nothing, leaves them on).
        let on = Settings::parse("N\nsingle_window=yes\n");
        assert!(on.single_window && on.remember_place);
        let off = Settings::parse("N\nsingle_window=0\nremember_place= 0 \n");
        assert!(!off.single_window && !off.remember_place);
        assert!(!Settings::parse(&off.render()).single_window);
        assert!(!Settings::parse(&Settings::default().render()).render().contains("single_window"));
        assert_eq!(Settings::parse("N\ntab=thumbnails\n").tab, None);
        // Too long a first line is not a name, and does not stop the rest.
        let long = format!("{}\nlang=en\n", "x".repeat(MAX_AUTHOR_CHARS + 1));
        let s = Settings::parse(&long);
        assert_eq!((s.author, s.lang), (None, LangChoice::En));
    }
}
