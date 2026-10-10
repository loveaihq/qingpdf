//! The speed test behind the hidden `--bench <file>` argument: the reader opens the file, goes through all its pages
//! by itself, writes down how long each step took and how much memory it used, prints that and quits. It drives the
//! real reader (window, scheduling, engine, drawing), so the numbers are what a person would see. A step ends when the
//! page has been drawn into the window's back buffer; the last copy to the screen is not timed, and the pictures that
//! are not the one waited for are not copied at all (on the test machine Windows holds a `BitBlt` to the screen for
//! half a second now and then, whatever is in it, and that is not the reader's time). `--bench-size WxH` gives the
//! window a client area of that size, so that a maximized window on a bigger screen can be tried.

use std::time::{Duration, Instant};

use crate::app::App;
use crate::ui::Action;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Going from page to page, each turn after everything wanted has come in (a person reads a page before turning).
    Reading,
    /// The same, but each turn right after the picture of the last one (the prefetch is cut short: holding Page Down).
    Rapid,
    /// Every bitmap is let go of before each page turn: the cost of a page with nothing ready.
    Cold,
}

enum State {
    /// Waiting for the first page to be on the screen.
    First,
    /// Going through the pages in one pass; `next` is the page to go to.
    Pass { pass: Pass, next: usize },
    Done,
}

pub struct Bench {
    state: State,
    file: String,
    size: Option<(i32, i32)>,
    first_any: Option<Duration>,
    first_sharp: Option<Duration>,
    pages: usize,
    /// When the turn being waited for began.
    turn_started: Option<Instant>,
    reading: Vec<f64>,
    rapid: Vec<f64>,
    cold: Vec<f64>,
    /// Turns that ended with the pieces in view drawn smaller than the zoom and stretched.
    reduced: usize,
    /// Turns (of all passes) whose pieces were all in the cache at the moment the turn began.
    ready: usize,
    deadline: Instant,
}

impl Bench {
    pub fn new(file: &str, size: Option<(i32, i32)>) -> Bench {
        Bench {
            state: State::First,
            file: file.to_string(),
            size,
            first_any: None,
            first_sharp: None,
            pages: 0,
            turn_started: None,
            reading: Vec::new(),
            rapid: Vec::new(),
            cold: Vec::new(),
            reduced: 0,
            ready: 0,
            deadline: Instant::now() + Duration::from_secs(15 * 60),
        }
    }

    /// Called after every event (and after each picture, which wakes the reader up): take the next step if the
    /// last one is done. Returns what the window should do.
    pub fn step(&mut self, app: &mut App) -> Vec<Action> {
        if Instant::now() > self.deadline && !matches!(self.state, State::Done) {
            println!("bench: gave up after 15 minutes");
            self.state = State::Done;
            return vec![Action::Quit];
        }
        match self.state {
            State::First => {
                if !app.has_document() {
                    return Vec::new();
                }
                if self.first_any.is_none() {
                    self.first_any = app.first_bitmap_painted().map(|t| t.duration_since(app.started));
                }
                let Some(t) = app.last_sharp_paint() else { return Vec::new() };
                self.first_sharp = Some(t.duration_since(app.started));
                self.pages = app.page_count();
                self.state = State::Pass { pass: Pass::Reading, next: 1 };
                self.step(app)
            }
            State::Pass { pass, next } => self.turn(app, pass, next),
            State::Done => Vec::new(),
        }
    }

    fn turn(&mut self, app: &mut App, pass: Pass, next: usize) -> Vec<Action> {
        // The turn being waited for is over when a picture with everything sharp has been drawn after it began.
        if let (Some(began), Some(painted)) = (self.turn_started, app.last_sharp_paint()) {
            let ms = painted.duration_since(began).as_secs_f64() * 1000.0;
            match pass {
                Pass::Reading => self.reading.push(ms),
                Pass::Rapid => self.rapid.push(ms),
                Pass::Cold => self.cold.push(ms),
            }
            if app.reduced() {
                self.reduced += 1;
            }
            self.turn_started = None;
        }
        if self.turn_started.is_some() {
            return Vec::new();
        }
        if next >= self.pages {
            // This pass is done.
            let following = match pass {
                Pass::Reading => Pass::Rapid,
                Pass::Rapid => Pass::Cold,
                Pass::Cold => return self.finish(),
            };
            self.state = State::Pass { pass: following, next: 1 };
            // Back to the first page for the next pass (not timed), then on.
            let actions = app.go_to_page(0);
            app.clear_last_sharp_paint();
            self.turn_started = None;
            let mut all = actions;
            all.extend(self.settle_then_turn(app, following, 1));
            return all;
        }
        self.settle_then_turn(app, pass, next)
    }

    /// Start the turn to page `next`, after waiting (for the reading pass) until nothing is asked of the engine.
    fn settle_then_turn(&mut self, app: &mut App, pass: Pass, next: usize) -> Vec<Action> {
        self.state = State::Pass { pass, next };
        if (pass == Pass::Reading || next == 1) && !(app.visible_ready() && app.is_idle()) {
            // Everything wanted is not in yet: the next event (the engine's answer) comes here again.
            return Vec::new();
        }
        if pass == Pass::Cold {
            app.forget_bitmaps();
        }
        self.state = State::Pass { pass, next: next + 1 };
        app.clear_last_sharp_paint();
        self.turn_started = Some(Instant::now());
        let actions = app.go_to_page(next);
        if app.visible_ready() {
            self.ready += 1;
        }
        actions
    }

    fn finish(&mut self) -> Vec<Action> {
        self.state = State::Done;
        let ms = |d: Option<Duration>| d.map_or_else(|| "never".to_string(), |d| format!("{:.0} ms", d.as_secs_f64() * 1000.0));
        println!("file: {}", self.file);
        match self.size {
            Some((w, h)) => println!("window: {w} x {h} pixels (client area)"),
            None => println!("window: default size"),
        }
        println!("pages: {}", self.pages);
        println!("time to the first page (something on the screen): {}", ms(self.first_any));
        println!("time to the first page (sharp): {}", ms(self.first_sharp));
        println!("page turns, reading one page at a time (prefetch finished): {}", summary(&self.reading));
        println!("page turns, one right after another (prefetch cut short): {}", summary(&self.rapid));
        println!("page turns, nothing ready (cold): {}", summary(&self.cold));
        println!("turns whose pieces were all there when the turn began: {}", self.ready);
        println!("turns drawn at a smaller scale and stretched: {}", self.reduced);
        let peak = crate::win32::peak_working_set();
        println!("peak working set: {:.1} MB ({} bytes)", peak as f64 / 1_000_000.0, peak);
        vec![Action::Quit]
    }
}

/// "n = 99, median 12 ms, 95th percentile 30 ms, max 41 ms".
fn summary(v: &[f64]) -> String {
    if v.is_empty() {
        return "none".to_string();
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| s.get(((s.len() - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0);
    format!("n = {}, median {:.1} ms, 95th percentile {:.1} ms, max {:.1} ms", s.len(), at(0.5), at(0.95), at(1.0))
}

#[cfg(test)]
mod tests {
    use super::summary;

    #[test]
    fn the_summary_gives_median_and_extremes() {
        assert_eq!(summary(&[]), "none");
        assert_eq!(summary(&[5.0]), "n = 1, median 5.0 ms, 95th percentile 5.0 ms, max 5.0 ms");
        let s = summary(&[10.0, 1.0, 3.0, 2.0, 100.0]);
        assert!(s.contains("median 3.0") && s.contains("max 100.0"), "{s}");
    }
}
