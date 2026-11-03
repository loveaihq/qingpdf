//! The command line of the Mac reader (the same as the Windows program's, `qingpdf-view-bin`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use qingpdf_view::app::{App, PrintTest};
use qingpdf_view::bench::Bench;
use qingpdf_view::layout::Spread;
use qingpdf_view::theme::{self, Tint};
use qingpdf_view::copydata;
use qingpdf_view::i18n::{Lang, Msg, t};
use qingpdf_view::settings;
use qingpdf_view::ui::{Action, Cursor, Event, Handler, NcHit, Painter};

use crate::macos as sys;

/// The hidden OCR test: the file, the file to write, the pages (`None`: every scan).
type OcrTest = (PathBuf, PathBuf, Option<(u32, u32)>);

/// "1920x1080" as a size in pixels.
fn parse_size(s: &str) -> Option<(i32, i32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    (w >= 100 && h >= 100).then_some((w, h))
}

/// "2-4" or "3" as the first and last page (from 1) of a range.
fn parse_pages(s: &str) -> Option<(u32, u32)> {
    let (a, b) = match s.split_once('-') {
        Some((a, b)) => (a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?),
        None => {
            let n = s.trim().parse::<u32>().ok()?;
            (n, n)
        }
    };
    (a >= 1 && b >= a).then_some((a, b))
}

/// A panic ends the program (the build aborts on panic), and a program of the window kind has no console to say so on:
/// show a box with the place of the panic first. Only the place is shown, never the message, which may quote the file.
fn show_panics(lang: Lang) {
    std::panic::set_hook(Box::new(move |info| {
        let place = info.location().map_or_else(|| "?".to_string(), |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        sys::fatal_box(t(lang, Msg::InternalErrorTitle), &format!("{} {place}", t(lang, Msg::InternalError)));
    }));
}

pub fn main() {
    qingpdf_view::host::set(qingpdf_view::host::Host { open_recognizer: sys::open_recognizer, peak_working_set: sys::peak_working_set });
    let started = Instant::now();
    let mut args = std::env::args_os().skip(1);
    let mut bench_file: Option<PathBuf> = None;
    let mut bench_size: Option<(i32, i32)> = None;
    // The speed test with the sidebar open on the thumbnails.
    let mut bench_thumbs = false;
    // The speed test with two pages to a row, in other colours, or in the presentation.
    let (mut bench_spread, mut bench_color, mut bench_present) = (false, Tint::Original, false);
    let mut open: Vec<std::ffi::OsString> = Vec::new();
    // The hidden print test: the file, the file to write, the pages.
    let mut print_test: Option<(PathBuf, PathBuf, (u32, u32))> = None;
    // The hidden OCR test: the file, the file to write, the pages (all the scans by default), Chinese or the person's languages.
    let mut ocr_test: Option<OcrTest> = None;
    let mut ocr_chinese: Option<bool> = None;
    while let Some(arg) = args.next() {
        if arg == "--bench" {
            bench_file = args.next().map(PathBuf::from);
        } else if arg == "--write-icon" {
            // Hidden: draws the mark as a PNG (`make-app.sh` makes the `.app`'s icon from it).
            let ok = args.next().is_some_and(|p| sys::write_icon(&p.to_string_lossy(), 1024));
            std::process::exit(if ok { 0 } else { 1 });
        } else if arg == "--bench-thumbs" {
            bench_thumbs = true;
        } else if arg == "--bench-spread" {
            bench_spread = true;
        } else if arg == "--bench-present" {
            bench_present = true;
        } else if arg == "--bench-color" {
            bench_color = args.next().and_then(|s| theme::parse_tint(&s.to_string_lossy())).unwrap_or(Tint::Original);
        } else if arg == "--bench-size" {
            bench_size = args.next().and_then(|s| parse_size(&s.to_string_lossy()));
        } else if arg == "--print-test" {
            let (file, out) = (args.next().map(PathBuf::from), args.next().map(PathBuf::from));
            let pages = args.next().and_then(|s| parse_pages(&s.to_string_lossy())).unwrap_or((1, 3));
            print_test = file.zip(out).map(|(f, o)| (f, o, pages));
        } else if arg == "--ocr-test" {
            let (file, out) = (args.next().map(PathBuf::from), args.next().map(PathBuf::from));
            ocr_test = file.zip(out).map(|(f, o)| (f, o, None));
        } else if arg == "--ocr-pages" {
            let pages = args.next().and_then(|s| parse_pages(&s.to_string_lossy()));
            if let Some(t) = ocr_test.as_mut() {
                t.2 = pages;
            }
        } else if arg == "--ocr-lang" {
            ocr_chinese = args.next().map(|s| s.to_string_lossy().to_ascii_lowercase().starts_with("zh"));
        } else if !arg.to_string_lossy().starts_with('-') {
            // (Not a switch: the system may add some of its own, like `-psn_0_12345` or `-NSDocumentRevisionsDebugMode`.)
            open.push(arg);
        }
    }
    if bench_file.is_some() || print_test.is_some() || ocr_test.is_some() {
        sys::attach_console();
    }
    let system_lang = Lang::from_langid(sys::ui_language());
    // What was kept between runs (the language, the toolbar, the width of the bookmarks, the window's place). The hidden tests start
    // from the usual and keep nothing.
    let tests = bench_file.is_some() || print_test.is_some() || ocr_test.is_some();
    let kept = if tests { settings::Settings::default() } else { settings::load_all() };
    let lang = kept.lang.resolve(system_lang);
    let open = copydata::paths_of(open);
    // One window for all the files: a copy that is running (or is just starting) takes the files and this one ends.
    if !tests && kept.single_window {
        let first = sys::first_copy();
        let mut tries = if first { 1 } else { 30 };
        while !open.is_empty() && tries > 0 {
            if sys::hand_over(&open) {
                std::process::exit(0);
            }
            tries -= 1;
            if tries > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    if let Some((file, out, pages)) = ocr_test {
        let chinese = ocr_chinese.unwrap_or(lang == Lang::Zh);
        std::process::exit(qingpdf_view::ocr::ocr_test(&file.to_string_lossy(), &out.to_string_lossy(), pages, chinese));
    }
    show_panics(lang);
    // The most pixels the window can show: the work area of the screen, or the size the speed test asked for.
    let (work_w, work_h) = sys::work_area_size();
    let screen = bench_size.unwrap_or((work_w, work_h));
    let max_view_pixels = u64::try_from(screen.0.max(0)).unwrap_or(0) * u64::try_from(screen.1.max(0)).unwrap_or(0);
    let code = sys::run("qingpdf", bench_size, kept.window.filter(|_| !tests), !tests, move |waker| {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.wake());
        // Who the annotations are by: the name that was chosen, else the Windows user's.
        let author = kept.author.clone().or_else(|| settings::clean(&sys::user_name())).unwrap_or_default();
        let mut app = App::new(system_lang, wake, started, max_view_pixels, author);
        app.apply_settings(&kept);
        app.set_system_dark(sys::system_uses_dark());
        app.set_system_accent(sys::system_accent());
        app.set_contrast(sys::high_contrast());
        app.set_clock(sys::local_date);
        app.set_local_time(sys::local_time);
        app.set_random(sys::random_bytes);
        // The window's own buttons (red, yellow, green) sit at the left end of the title strip.
        app.set_left_inset(sys::BUTTONS_ROOM);
        if let Some(file) = bench_file {
            app.bench = Some(Bench::new(&file.to_string_lossy(), bench_size));
            if bench_thumbs {
                app.bench_thumbs();
            }
            app.bench_looks(if bench_spread { Spread::Pair } else { Spread::Single }, bench_color, bench_present);
            app.open_when_ready(file);
        } else if let Some((file, output, (first, last))) = print_test {
            app.set_print_test(PrintTest { output, first, last });
            app.open_when_ready(file);
        } else {
            app.open_all_when_ready(open);
        }
        Box::new(MacReader { app, full: false })
    });
    std::process::exit(code);
}

/// The reader with the one thing only the Mac needs: the room for the window's buttons at the left of the title strip goes in full screen,
/// where the buttons are gone, and comes back after.
struct MacReader {
    app: App,
    full: bool,
}

impl Handler for MacReader {
    fn event(&mut self, event: Event) -> Vec<Action> {
        let full = sys::full_screen();
        let changed = full != self.full;
        if changed {
            self.full = full;
            self.app.set_left_inset(if full { 0 } else { sys::BUTTONS_ROOM });
        }
        let mut actions = self.app.event(event);
        if changed {
            actions.push(Action::Invalidate);
        }
        actions
    }
    fn paint(&mut self, painter: &mut dyn Painter) -> bool {
        self.app.paint(painter)
    }
    fn paint_part(&mut self, painter: &mut dyn Painter, x: i32, y: i32) -> bool {
        self.app.paint_part(painter, x, y)
    }
    fn cursor_at(&mut self, x: i32, y: i32) -> Cursor {
        self.app.cursor_at(x, y)
    }
    fn nc_hit(&mut self, x: i32, y: i32) -> NcHit {
        self.app.nc_hit(x, y)
    }
    fn unsaved_reason(&self) -> Option<String> {
        self.app.unsaved_reason()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_pages, parse_size};

    #[test]
    fn a_size_is_width_x_height() {
        assert_eq!(parse_size("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_size("3840X2160"), Some((3840, 2160)));
        assert_eq!(parse_size("1920"), None);
        assert_eq!(parse_size("axb"), None);
        assert_eq!(parse_size("10x10"), None);
    }

    #[test]
    fn pages_are_a_number_or_a_range() {
        assert_eq!(parse_pages("2-4"), Some((2, 4)));
        assert_eq!(parse_pages("3"), Some((3, 3)));
        assert_eq!(parse_pages("4-2"), None);
        assert_eq!(parse_pages("0"), None);
        assert_eq!(parse_pages("a-b"), None);
    }
}
