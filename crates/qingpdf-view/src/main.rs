//! qingpdf-view: the reader. A window around the engine of `qingpdf-core`: open a file, scroll, zoom, turn pages.
//!
//! Layout of the program: `app.rs` is the reader (state, scheduling, drawing) and knows nothing of Windows;
//! `win32.rs` is the one module that calls Windows (and the only one with `unsafe`); `ui.rs` is the plain-types
//! contract between them. `layout`, `zoom`, `tiles`, `cache` and `sched` are plain logic with tests.
//!
//! Run `qingpdf-view [file.pdf]`. The hidden `--bench <file.pdf>` opens the file, goes through its pages by itself,
//! prints times and the memory peak and quits; `--bench-size WxH` gives the window of that test a client area of that
//! many pixels (it may be bigger than the screen).

#![windows_subsystem = "windows"]

mod app;
mod bench;
mod cache;
mod i18n;
mod layout;
mod sched;
mod tiles;
mod ui;
#[allow(unsafe_code)]
mod win32;
mod zoom;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use app::App;
use bench::Bench;
use i18n::{Lang, Msg, t};

/// "1920x1080" as a size in pixels.
fn parse_size(s: &str) -> Option<(i32, i32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    (w >= 100 && h >= 100).then_some((w, h))
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
    let mut open: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--bench" {
            bench_file = args.next().map(PathBuf::from);
        } else if arg == "--bench-size" {
            bench_size = args.next().and_then(|s| parse_size(&s.to_string_lossy()));
        } else if open.is_none() && !arg.to_string_lossy().starts_with("--") {
            open = Some(PathBuf::from(arg));
        }
    }
    if bench_file.is_some() {
        win32::attach_console();
    }
    let lang = Lang::from_langid(win32::ui_language());
    show_panics(lang);
    let menus = App::menus(lang);
    // The most pixels the window can show: the work area of the screen, or the size the speed test asked for.
    let (work_w, work_h) = win32::work_area_size();
    let screen = bench_size.unwrap_or((work_w, work_h));
    let max_view_pixels = u64::try_from(screen.0.max(0)).unwrap_or(0) * u64::try_from(screen.1.max(0)).unwrap_or(0);
    let code = win32::run("qingpdf", &menus, bench_size, move |waker| {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || waker.wake());
        let mut app = App::new(lang, wake, started, max_view_pixels);
        if let Some(file) = bench_file {
            app.bench = Some(Bench::new(&file.to_string_lossy(), bench_size));
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
    use super::parse_size;

    #[test]
    fn a_size_is_width_x_height() {
        assert_eq!(parse_size("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_size("3840X2160"), Some((3840, 2160)));
        assert_eq!(parse_size("1920"), None);
        assert_eq!(parse_size("axb"), None);
        assert_eq!(parse_size("10x10"), None);
    }
}
