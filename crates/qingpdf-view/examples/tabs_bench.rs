//! Speed and memory of several files in one window (2026-10-17, docs/decisions.md): the reader is driven without a window (the
//! way the unit tests drive it, with a real engine), each file is opened in a tab, and then the tabs are shown one after the other;
//! for each the time from the command (Ctrl+Tab) to the page being on the screen is told (the first picture of it in the window's
//! painter), and the time to every piece of it in view being sharp, with the memory peak.
//!
//!   APPDATA=some-folder cargo run --release --example tabs_bench -p qingpdf-view -- [--goal MB] [--rounds N] file.pdf ...
//!
//! `--goal` sets the memory goal handed to the engine for all the files together (200 by default); a small one makes the engine let
//! go of the tabs that wait, so that showing one reads its file again. Set APPDATA to a folder of your own: the program keeps its
//! settings and its list of files there.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use qingpdf_view::app::{App, CMD_NEXT_TAB};
use qingpdf_view::i18n::Lang;
use qingpdf_view::ui::{Event, Handler, Painter, Rect};

type Signal = Arc<(Mutex<bool>, Condvar)>;

/// A painter that only counts the pictures it is asked to draw.
#[derive(Default)]
struct Counter {
    pictures: usize,
}

impl Painter for Counter {
    fn fill_rect(&mut self, _: Rect, _: u32) {}
    fn fill_rect_alpha(&mut self, _: Rect, _: u32, _: u8) {}
    fn draw_bgra(&mut self, _: Rect, _: i32, _: i32, _: &[u8]) {
        self.pictures += 1;
    }
    fn text(&mut self, _: i32, _: i32, _: &str, _: u32) {}
    fn text_width(&mut self, s: &str) -> i32 {
        7 * s.chars().count() as i32
    }
    fn line_height(&self) -> i32 {
        16
    }
}

/// The loop of the window, as far as the reader sees it: wait to be woken, then hear that the engine has something to say.
/// `step` is given the reader after each time; the time to the first `true` of `until` is returned.
fn wait_until(app: &mut App, signal: &Signal, what: &str, mut until: impl FnMut(&mut App) -> bool) -> Duration {
    let started = Instant::now();
    loop {
        app.event(Event::Wake);
        if until(app) {
            return started.elapsed();
        }
        assert!(started.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
        let (lock, cv) = &**signal;
        let mut woken = lock.lock().unwrap();
        if !*woken {
            woken = cv.wait_timeout(woken, Duration::from_millis(20)).unwrap().0;
        }
        *woken = false;
    }
}

fn stats(label: &str, mut ms: Vec<f64>) {
    if ms.is_empty() {
        return;
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| ms[((ms.len() - 1) as f64 * p).round() as usize];
    println!("{label}: {} times, median {:.1} ms, 95% {:.1} ms, max {:.1} ms", ms.len(), at(0.5), at(0.95), at(1.0));
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.0
}

fn main() {
    let (mut goal, mut rounds, mut files): (Option<u64>, usize, Vec<PathBuf>) = (None, 3, Vec::new());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--goal" => goal = args.next().and_then(|v| v.parse::<u64>().ok()).map(|m| m * 1_000_000),
            "--rounds" => rounds = args.next().and_then(|v| v.parse().ok()).unwrap_or(3),
            _ => files.push(PathBuf::from(a)),
        }
    }
    let signal: Signal = Arc::new((Mutex::new(false), Condvar::new()));
    let for_engine = signal.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let (lock, cv) = &*for_engine;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    });
    let mut app = App::new(Lang::En, wake, Instant::now(), 1920 * 1080, "bench".to_string());
    if let Some(g) = goal {
        app.set_memory_goal(g);
    }
    app.event(Event::Size { w: 1920, h: 1000 });
    let mut opening = Vec::new();
    for f in &files {
        app.open_path(f, "");
        let t = wait_until(&mut app, &signal, "the file to open", |a| a.has_document() && a.visible_ready());
        opening.push(t.as_secs_f64() * 1000.0);
        println!("opened {} in {:.0} ms; peak so far {:.1} MB", f.file_name().unwrap().to_string_lossy(), t.as_secs_f64() * 1000.0, mb(qingpdf_view::win32::peak_working_set()));
    }
    stats("opening a file in a new tab (to its first page drawn sharp)", opening);
    let (mut shown, mut sharp) = (Vec::new(), Vec::new());
    for _ in 0..rounds {
        for _ in 0..files.len() {
            let started = Instant::now();
            app.event(Event::Command(CMD_NEXT_TAB));
            let mut first = None;
            wait_until(&mut app, &signal, "the tab to be drawn", |a| {
                if first.is_none() {
                    let mut count = Counter::default();
                    a.paint(&mut count);
                    if count.pictures > 0 {
                        first = Some(started.elapsed());
                    }
                }
                a.has_document() && a.visible_ready()
            });
            sharp.push(started.elapsed().as_secs_f64() * 1000.0);
            shown.push(first.map_or(f64::NAN, |d| d.as_secs_f64() * 1000.0));
        }
    }
    shown.retain(|v| v.is_finite());
    stats("showing a tab: the page on the screen (command to its first picture)", shown);
    stats("showing a tab: every piece in view sharp", sharp);
    println!("peak working set {:.1} MB (goal {} MB)", mb(qingpdf_view::win32::peak_working_set()), goal.map_or(200, |g| g / 1_000_000));
}
