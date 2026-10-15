//! qingpdf-view: the reader. A window around the engine of `qingpdf-core`: open a file, scroll, zoom, turn pages.
//!
//! The program is only the command line (and the manifest, `build.rs`): the reader is the library `qingpdf-view`.
//!
//! Run `qingpdf-view [file.pdf]`. The hidden `--bench <file.pdf>` opens the file, goes through its pages by itself,
//! prints times and the memory peak and quits; `--bench-size WxH` gives the window of that test a client area of that
//! many pixels (it may be bigger than the screen); `--bench-thumbs` opens the sidebar on the thumbnails in it. The hidden `--print-test <file.pdf> <out.pdf> [first-last]` prints those pages
//! (the first three by default) to the printer "Microsoft Print to PDF", which writes `out.pdf`, with no box, says how it went
//! and the memory peak, and quits. The hidden `--ocr-test <in.pdf> <out.pdf> [--ocr-pages first-last] [--ocr-lang zh|en]` recognises the text of
//! the scanned pages with the same chain the window uses, saves the file with the text layer, prints how long each page took and quits.

#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use qingpdf_view::app::{App, PrintTest};
use qingpdf_view::bench::Bench;
use qingpdf_view::i18n::{Lang, Msg, t};
use qingpdf_view::{settings, win32};

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
        win32::fatal_box(t(lang, Msg::InternalErrorTitle), &format!("{} {place}", t(lang, Msg::InternalError)));
    }));
}

fn main() {
    let started = Instant::now();
    let mut args = std::env::args_os().skip(1);
    let mut bench_file: Option<PathBuf> = None;
    let mut bench_size: Option<(i32, i32)> = None;
    // The speed test with the sidebar open on the thumbnails.
    let mut bench_thumbs = false;
    let mut open: Option<PathBuf> = None;
    // The hidden print test: the file, the file to write, the pages.
    let mut print_test: Option<(PathBuf, PathBuf, (u32, u32))> = None;
    // The hidden OCR test: the file, the file to write, the pages (all the scans by default), Chinese or the person's languages.
    let mut ocr_test: Option<OcrTest> = None;
    let mut ocr_chinese: Option<bool> = None;
    while let Some(arg) = args.next() {
        if arg == "--bench" {
            bench_file = args.next().map(PathBuf::from);
        } else if arg == "--bench-thumbs" {
            bench_thumbs = true;
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
        } else if open.is_none() && !arg.to_string_lossy().starts_with("--") {
            open = Some(PathBuf::from(arg));
        }
    }
    if bench_file.is_some() || print_test.is_some() || ocr_test.is_some() {
        win32::attach_console();
    }
    let system_lang = Lang::from_langid(win32::ui_language());
    // What was kept between runs (the language, the toolbar, the width of the bookmarks, the window's place). The hidden tests start
    // from the usual and keep nothing.
    let tests = bench_file.is_some() || print_test.is_some() || ocr_test.is_some();
    let kept = if tests { settings::Settings::default() } else { settings::load_all() };
    let lang = kept.lang.resolve(system_lang);
    if let Some((file, out, pages)) = ocr_test {
        let chinese = ocr_chinese.unwrap_or(lang == Lang::Zh);
        std::process::exit(qingpdf_view::ocr::ocr_test(&file.to_string_lossy(), &out.to_string_lossy(), pages, chinese));
    }
    show_panics(lang);
    let menus = App::initial_menus(lang);
    // The most pixels the window can show: the work area of the screen, or the size the speed test asked for.
    let (work_w, work_h) = win32::work_area_size();
    let screen = bench_size.unwrap_or((work_w, work_h));
    let max_view_pixels = u64::try_from(screen.0.max(0)).unwrap_or(0) * u64::try_from(screen.1.max(0)).unwrap_or(0);
    let code = win32::run("qingpdf", &menus, bench_size, kept.window.filter(|_| !tests), !tests, move |waker| {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.wake());
        // Who the annotations are by: the name that was chosen, else the Windows user's.
        let author = kept.author.clone().or_else(|| settings::clean(&win32::user_name())).unwrap_or_default();
        let mut app = App::new(system_lang, wake, started, max_view_pixels, author);
        app.apply_settings(&kept);
        app.set_clock(win32::local_date);
        if let Some(file) = bench_file {
            app.bench = Some(Bench::new(&file.to_string_lossy(), bench_size));
            if bench_thumbs {
                app.bench_thumbs();
            }
            app.open_when_ready(file);
        } else if let Some((file, output, (first, last))) = print_test {
            app.set_print_test(PrintTest { output, first, last });
            app.open_when_ready(file);
        } else if let Some(file) = open {
            app.open_when_ready(file);
        }
        Box::new(app)
    });
    std::process::exit(code);
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
