//! The tabs (2026-10-17): the list of the files that are open in the window, which one is shown, and the plain arithmetic of the strip
//! of tabs above the pages (how wide a tab is, what a click on it is). No Windows and no engine here: `app/tabbar.rs` draws the strip and
//! `app.rs` keeps the state of the tabs that are not shown in the list.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::places::same_file;

/// Most files open at once.
pub const MAX_TABS: usize = 16;
/// Most files that are looked at from one drop on the window (or kept waiting while a box is open); the rest of a longer list is left.
pub const MAX_DROPPED: usize = 32;
/// Most names of files that were not opened that one message lists (it says how many more there are).
pub const SKIPPED_SHOWN: usize = 8;
/// How long, in all, the first kilobytes of the files that were dropped may take to be read.
pub const SNIFF_WAIT: Duration = Duration::from_secs(2);

/// Where the document of a tab is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    /// The tab is shown: its state is the reader's own.
    Shown,
    /// The tab is not shown and its document waits in the engine (with its edits that are not saved, if it has some).
    Waiting,
    /// The tab is not shown and the engine let go of its document: it is read from the disk again when the tab is wanted.
    Dropped,
}

/// One tab. `S` is what the reader keeps of a tab that is not shown.
pub struct Tab<S> {
    pub path: PathBuf,
    /// The document has changes that are not saved (the tab says so).
    pub dirty: bool,
    /// When it was last shown (a count of the changes of tab).
    pub left: u64,
    pub at: Where,
    pub kept: Option<Box<S>>,
}

/// The tabs, in the order they stand in the strip.
pub struct Tabs<S> {
    list: Vec<Tab<S>>,
    active: usize,
}

/// What closing a tab leaves: the tab itself and, if it was the shown one, the one to show instead (an index into the list that is left).
pub struct Closed<S> {
    pub tab: Tab<S>,
    pub show: Option<usize>,
}

impl<S> Default for Tabs<S> {
    fn default() -> Tabs<S> {
        Tabs { list: Vec::new(), active: 0 }
    }
}

impl<S> Tabs<S> {
    pub fn new() -> Tabs<S> {
        Tabs::default()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn full(&self) -> bool {
        self.list.len() >= MAX_TABS
    }

    /// The tab that is shown (none when there is no tab).
    pub fn active(&self) -> Option<usize> {
        (!self.list.is_empty()).then_some(self.active)
    }

    pub fn get(&self, index: usize) -> Option<&Tab<S>> {
        self.list.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Tab<S>> {
        self.list.get_mut(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab<S>> {
        self.list.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab<S>> {
        self.list.iter_mut()
    }

    /// The tab of this file, if it is open (the same file by another spelling counts).
    pub fn find(&self, path: &Path) -> Option<usize> {
        self.list.iter().position(|t| same_file(&t.path, path))
    }

    /// The tab whose document has this engine number.
    pub fn position(&self, is: impl FnMut(&Tab<S>) -> bool) -> Option<usize> {
        self.list.iter().position(is)
    }

    /// A new tab at the end for this file, shown. `None`: there are [`MAX_TABS`] already.
    pub fn push(&mut self, path: PathBuf) -> Option<usize> {
        if self.full() {
            return None;
        }
        self.list.push(Tab { path, dirty: false, left: 0, at: Where::Shown, kept: None });
        self.active = self.list.len() - 1;
        Some(self.active)
    }

    /// Show tab `index`.
    pub fn show(&mut self, index: usize) {
        if index < self.list.len() {
            self.active = index;
        }
    }

    /// The tab after (`forward`) or before the shown one, the strip being a ring. `None`: there is no other tab.
    pub fn next(&self, forward: bool) -> Option<usize> {
        let n = self.list.len();
        (n >= 2).then(|| if forward { (self.active + 1) % n } else { (self.active + n - 1) % n })
    }

    /// Take tab `index` out. If it was the shown one the tab that took its place (the one to its right, else the last) is the one to show;
    /// the number of the shown tab is kept right for the others.
    pub fn close(&mut self, index: usize) -> Option<Closed<S>> {
        if index >= self.list.len() {
            return None;
        }
        let tab = self.list.remove(index);
        let was_shown = index == self.active;
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.list.len().saturating_sub(1));
        Some(Closed { tab, show: (was_shown && !self.list.is_empty()).then_some(self.active) })
    }

    /// The first tab holding changes that are not saved, leaving out those `skip` says to.
    pub fn first_dirty(&self, skip: impl Fn(&Tab<S>) -> bool) -> Option<usize> {
        self.list.iter().position(|t| t.dirty && !skip(t))
    }
}

// --- the strip ---------------------------------------------------------------------------------------------------

/// Sizes of the title strip at 96 dpi: its height, the mark (the logo) and its room to the left, the gap above a tab, the width a tab may have, the gap
/// between tabs, the button that opens another file ("+"), the button that closes a tab and the room to its right, and the least of the strip that is left
/// for dragging the window (to the left of the buttons of the frame).
pub const TITLE_H: i64 = 40;
pub const MARK: i64 = 20;
pub const MARK_X: i64 = 14;
pub const TAB_TOP: i64 = 6;
pub const MIN_TAB_W: i64 = 44;
pub const MAX_TAB_W: i64 = 220;
pub const TAB_GAP: i64 = 2;
pub const PLUS: i64 = 28;
pub const PLUS_GAP: i64 = 4;
pub const CLOSE_SIDE: i64 = 20;
pub const CLOSE_EDGE: i64 = 6;
pub const DRAG_MIN: i64 = 48;
/// A tab narrower than this shows its close button only when it is the shown one or the pointer is on it.
pub const WIDE_TAB: i64 = 120;

/// Where the parts of the title strip are, in pixels, from the number of tabs, the dots per inch and the edge the strip may not go past (the left
/// edge of the buttons of the frame, less what is kept for dragging).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Strip {
    /// The mark: its left edge, its top and its side.
    pub mark: (i64, i64, i64),
    /// Where tabs begin, their top and height, and each tab's left edge, all of one width.
    pub first: i64,
    pub top: i64,
    pub height: i64,
    pub width: i64,
    pub xs: Vec<i64>,
    /// The "+": its left edge, its top and its side (none without a tab: the welcome page has the button).
    pub plus: Option<(i64, i64, i64)>,
    /// The close button's side and its distance from the right edge of a tab.
    pub close: (i64, i64),
}

impl Strip {
    pub fn new(count: usize, dpi: u32, right: i64) -> Strip {
        let s = |v: i64| v * i64::from(dpi.max(1)) / 96;
        let first = s(MARK_X + MARK + 10);
        let n = i64::try_from(count).unwrap_or(0);
        let reserved = if n > 0 { s(PLUS + PLUS_GAP) } else { 0 };
        let room = (right - first - reserved - (n - 1).max(0) * s(TAB_GAP)).max(0);
        let width = if n > 0 { (room / n).clamp(s(MIN_TAB_W), s(MAX_TAB_W)) } else { s(MAX_TAB_W) };
        let xs: Vec<i64> = (0..n).map(|i| first + i * (width + s(TAB_GAP))).collect();
        let top = s(TAB_TOP);
        let height = s(TITLE_H) - top;
        let end = if n > 0 { first + n * (width + s(TAB_GAP)) - s(TAB_GAP) } else { first };
        let plus = (n > 0).then(|| (end + s(PLUS_GAP), top + (height - s(PLUS)) / 2, s(PLUS)));
        Strip { mark: (s(MARK_X), (s(TITLE_H) - s(MARK)) / 2, s(MARK)), first, top, height, width, xs, plus, close: (s(CLOSE_SIDE), s(CLOSE_EDGE)) }
    }

    /// The close button of tab `i`: left, top, side.
    pub fn close_box(&self, i: usize) -> Option<(i64, i64, i64)> {
        let x = *self.xs.get(i)?;
        Some((x + self.width - self.close.1 - self.close.0, self.top + (self.height - self.close.0) / 2, self.close.0))
    }
}

/// What a place of the title strip is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    /// The button that closes the tab.
    Close(usize),
    /// The button that opens a file in a tab of its own.
    Plus,
    /// Nothing: the strip is where the window is dragged.
    Nothing,
}

/// What is at (`x`, `y`) of the strip; `close_shown(i)` says whether the button of tab `i` is there.
pub fn hit(strip: &Strip, x: i64, y: i64, close_shown: impl Fn(usize) -> bool) -> Hit {
    let inside = |(l, t, side): (i64, i64, i64)| x >= l && x < l + side && y >= t && y < t + side;
    if strip.plus.is_some_and(inside) {
        return Hit::Plus;
    }
    if y < strip.top || x < strip.first {
        return Hit::Nothing;
    }
    for (i, &left) in strip.xs.iter().enumerate() {
        if x >= left && x < left + strip.width {
            return if close_shown(i) && strip.close_box(i).is_some_and(inside) { Hit::Close(i) } else { Hit::Tab(i) };
        }
    }
    Hit::Nothing
}

/// Cut `text` so that it fits `fits` (a test of a width the caller knows how to measure), with an ellipsis at the end when it was cut.
pub fn shorten_to(text: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    for keep in (0..chars.len()).rev() {
        let cut: String = chars.iter().take(keep).chain(std::iter::once(&'\u{2026}')).collect();
        if fits(&cut) {
            return cut;
        }
    }
    String::new()
}

/// Does the file at `path` look like a PDF file: `%PDF-` in its first 1024 bytes (where the format puts it)? An error if it cannot be read. (A
/// folder or a file that is not there cannot be read.)
pub fn looks_like_pdf(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    if is_device_path(path) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file"));
    }
    let mut head = Vec::new();
    std::fs::File::open(path)?.take(1024).read_to_end(&mut head)?;
    Ok(head.windows(5).any(|w| w == b"%PDF-"))
}

/// A device (`\\.\pipe\x`) or a path spelt the long way (`\\?\C:\x`): not opened from outside, since reading some of them waits for ever.
pub fn is_device_path(path: &Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    text.starts_with("\\\\.\\") || text.starts_with("\\\\?\\")
}

/// What a look at a file, made on a thread of its own, gives.
pub enum Looked {
    /// Whether the first kilobyte of the file says it is a PDF file ([`looks_like_pdf`]).
    Pdf(std::io::Result<bool>),
    /// The file's size and time, if it can be looked at.
    Stamp(Option<crate::places::Stamp>),
}

/// A look at a file, to be made on a thread.
pub type Look = Box<dyn FnOnce() -> Looked + Send>;

/// Make these looks all at once (a thread each) and wait at most `wait` in all for them. The answer of one that is not back by then is `None`: a
/// file on a share that does not answer must not hold up the window (its thread is left to end whenever the system lets it).
pub fn look_all(jobs: Vec<Look>, wait: Duration) -> Vec<Option<Looked>> {
    let (send, receive) = std::sync::mpsc::channel();
    let mut answers: Vec<Option<Looked>> = Vec::new();
    for (i, job) in jobs.into_iter().enumerate() {
        answers.push(None);
        let send = send.clone();
        let _ = std::thread::Builder::new().spawn(move || drop(send.send((i, job()))));
    }
    drop(send);
    let end = Instant::now() + wait;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let Ok((i, answer)) = receive.recv_timeout(left) else { break };
        if let Some(slot) = answers.get_mut(i) {
            *slot = Some(answer);
        }
    }
    answers
}

/// [`looks_like_pdf`] for each of these files, looked at together within `wait` in all; a file that has not answered by then is an error
/// (`TimedOut`).
pub fn sniff(files: &[PathBuf], wait: Duration) -> Vec<std::io::Result<bool>> {
    let jobs = files
        .iter()
        .map(|file| {
            let file = file.clone();
            Box::new(move || Looked::Pdf(looks_like_pdf(&file))) as Look
        })
        .collect();
    look_all(jobs, wait)
        .into_iter()
        .map(|answer| match answer {
            Some(Looked::Pdf(found)) => found,
            _ => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer")),
        })
        .collect()
}

/// The name of a file as the tab shows it: the last part of the path, or the whole path if it has no last part.
pub fn label(path: &Path) -> String {
    path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(names: &[&str]) -> Tabs<()> {
        let mut t = Tabs::new();
        for n in names {
            t.push(PathBuf::from(format!("C:\\docs\\{n}.pdf"))).unwrap();
        }
        t
    }

    #[test]
    fn a_new_tab_is_added_at_the_end_and_shown_up_to_sixteen() {
        let mut t = tabs(&["a", "b"]);
        assert_eq!((t.len(), t.active()), (2, Some(1)));
        t.show(0);
        assert_eq!(t.push(PathBuf::from("C:\\c.pdf")), Some(2));
        assert_eq!(t.active(), Some(2));
        while t.len() < MAX_TABS {
            t.push(PathBuf::from(format!("C:\\x{}.pdf", t.len()))).unwrap();
        }
        assert!(t.full());
        assert_eq!(t.push(PathBuf::from("C:\\one-too-many.pdf")), None);
        assert_eq!((t.len(), t.active()), (MAX_TABS, Some(MAX_TABS - 1)));
    }

    #[test]
    fn a_file_that_is_open_is_found_by_any_spelling_of_its_path() {
        let t = tabs(&["a", "b", "c"]);
        assert_eq!(t.find(Path::new("C:\\docs\\b.pdf")), Some(1));
        assert_eq!(t.find(Path::new("c:\\DOCS\\B.PDF")), Some(1));
        assert_eq!(t.find(Path::new("C:\\docs\\.\\b.pdf")), Some(1));
        assert_eq!(t.find(Path::new("C:\\docs\\d.pdf")), None);
        assert_eq!(t.find(Path::new("")), None);
    }

    #[test]
    fn next_and_previous_go_round_and_there_is_nowhere_to_go_with_one_tab() {
        let mut t = tabs(&["a", "b", "c"]);
        assert_eq!(t.next(true), Some(0));
        assert_eq!(t.next(false), Some(1));
        t.show(0);
        assert_eq!(t.next(false), Some(2));
        assert_eq!(t.next(true), Some(1));
        assert_eq!(tabs(&["a"]).next(true), None);
        assert_eq!(Tabs::<()>::new().next(false), None);
    }

    #[test]
    fn closing_the_shown_tab_shows_the_one_to_its_right_else_the_last() {
        let mut t = tabs(&["a", "b", "c", "d"]);
        t.show(1);
        let closed = t.close(1).unwrap();
        assert_eq!((closed.show, t.active(), t.len()), (Some(1), Some(1), 3), "c took its place");
        assert_eq!(t.get(1).unwrap().path, PathBuf::from("C:\\docs\\c.pdf"));
        t.show(2);
        let closed = t.close(2).unwrap();
        assert_eq!((closed.show, t.active()), (Some(1), Some(1)), "the last one went: the one before it is shown");
        // A tab to the left of the shown one goes: the shown one is the same tab with a smaller number. One to its right: no change.
        let mut t = tabs(&["a", "b", "c", "d"]);
        t.show(2);
        assert_eq!(t.close(0).unwrap().show, None);
        assert_eq!(t.active(), Some(1));
        assert_eq!(t.get(1).unwrap().path, PathBuf::from("C:\\docs\\c.pdf"));
        assert_eq!(t.close(2).unwrap().show, None);
        assert_eq!(t.active(), Some(1));
        // The last one: nothing is shown.
        let mut t = tabs(&["a"]);
        let closed = t.close(0).unwrap();
        assert!(closed.show.is_none() && t.is_empty() && t.active().is_none());
        assert!(t.close(0).is_none());
    }

    #[test]
    fn the_tabs_with_changes_are_found() {
        let mut t = tabs(&["a", "b", "c"]);
        assert_eq!(t.first_dirty(|_| false), None);
        t.get_mut(2).unwrap().dirty = true;
        t.get_mut(1).unwrap().dirty = true;
        assert_eq!(t.first_dirty(|_| false), Some(1));
        // Those that are left out (their changes were given up) are not found.
        assert_eq!(t.first_dirty(|tab| tab.path.ends_with("b.pdf")), Some(2));
        assert_eq!(t.first_dirty(|_| true), None);
    }

    #[test]
    fn tabs_share_the_strip_between_the_least_and_the_most_width() {
        let right = 1000;
        assert_eq!(Strip::new(1, 96, right).width, MAX_TAB_W);
        assert_eq!(Strip::new(2, 96, right).width, MAX_TAB_W);
        // Five tabs: what is left after the mark, the "+" and the gaps, shared.
        let five = Strip::new(5, 96, right);
        assert_eq!(five.width, (right - 44 - (PLUS + PLUS_GAP) - 4 * TAB_GAP) / 5);
        assert_eq!(five.xs, (0..5).map(|i| 44 + i * (five.width + TAB_GAP)).collect::<Vec<_>>());
        assert!(Strip::new(16, 96, right).width >= MIN_TAB_W && Strip::new(16, 96, right).width < 60);
        assert_eq!(Strip::new(16, 96, 400).width, MIN_TAB_W);
        assert_eq!(Strip::new(0, 96, right).width, MAX_TAB_W);
        assert_eq!(Strip::new(3, 96, -5).width, MIN_TAB_W);
        // Everything is in proportion at 150 dpi.
        let big = Strip::new(2, 144, 1500);
        assert_eq!((big.width, big.top, big.height), (MAX_TAB_W * 3 / 2, TAB_TOP * 3 / 2, (TITLE_H - TAB_TOP) * 3 / 2));
        // The "+" follows the last tab, and a strip with no tab has none.
        let two = Strip::new(2, 96, right);
        let (px, _, side) = two.plus.unwrap();
        assert_eq!((px, side), (two.xs[1] + two.width + PLUS_GAP, PLUS));
        assert!(Strip::new(0, 96, right).plus.is_none());
    }

    #[test]
    fn a_click_is_on_a_tab_on_its_close_button_on_the_plus_or_on_nothing() {
        let strip = Strip::new(3, 96, 1000);
        let shown = |i: usize| i == 1;
        let (w, y) = (strip.width, strip.top + strip.height / 2);
        let (t0, t1) = (strip.xs[0], strip.xs[1]);
        assert_eq!(hit(&strip, t0 + 10, y, shown), Hit::Tab(0));
        assert_eq!(hit(&strip, t1 + 30, y, shown), Hit::Tab(1));
        // The button of tab 1 is 20 square, 6 from its right edge, in the middle of its height.
        let (cl, ct, side) = strip.close_box(1).unwrap();
        assert_eq!((cl, side), (t1 + w - CLOSE_EDGE - CLOSE_SIDE, CLOSE_SIDE));
        assert_eq!(hit(&strip, cl + side / 2, ct + side / 2, shown), Hit::Close(1));
        assert_eq!(hit(&strip, cl, ct, shown), Hit::Close(1));
        assert_eq!(hit(&strip, cl - 1, y, shown), Hit::Tab(1));
        assert_eq!(hit(&strip, cl + side, y, shown), Hit::Tab(1));
        // The button of a tab that does not show one is not there.
        assert_eq!(hit(&strip, strip.xs[0] + w - CLOSE_EDGE - 5, y, shown), Hit::Tab(0));
        // The gap between tabs, above them, left of them, right of the "+": nothing (the window is dragged there).
        assert_eq!(hit(&strip, t1 - 1, y, shown), Hit::Nothing);
        assert_eq!(hit(&strip, t0 + 10, strip.top - 1, shown), Hit::Nothing);
        assert_eq!(hit(&strip, strip.first - 1, y, shown), Hit::Nothing);
        let (pl, pt, ps) = strip.plus.unwrap();
        assert_eq!(hit(&strip, pl + ps / 2, pt + ps / 2, shown), Hit::Plus);
        assert_eq!(hit(&strip, pl + ps + 40, y, shown), Hit::Nothing);
        assert_eq!(hit(&strip, i64::MAX, y, shown), Hit::Nothing);
        assert_eq!(hit(&Strip::new(0, 96, 1000), 100, y, |_| true), Hit::Nothing);
    }

    #[test]
    fn a_file_looks_like_a_pdf_by_its_first_kilobyte() {
        let dir = std::env::temp_dir().join(format!("qingpdf-tabs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        assert!(looks_like_pdf(&write("a.pdf", b"%PDF-1.7
1 0 obj")).unwrap());
        assert!(looks_like_pdf(&write("junk.pdf", b"MZ junk before the header %PDF-1.4")).unwrap());
        assert!(!looks_like_pdf(&write("b.pdf", b"hello world")).unwrap());
        assert!(!looks_like_pdf(&write("c.pdf", b"")).unwrap());
        assert!(!looks_like_pdf(&write("d.pdf", &[[b' '; 1100].as_slice(), b"%PDF-1.4"].concat())).unwrap(), "too far from the start");
        assert!(looks_like_pdf(&dir.join("missing.pdf")).is_err());
        assert!(looks_like_pdf(&dir).is_err());
        // Devices and the long spelling of a path are not looked at (a pipe nobody writes to would hold the window up).
        for device in ["\\\\.\\pipe\\x.pdf", "\\\\.\\GLOBALROOT\\Device\\x.pdf", "\\\\?\\C:\\x.pdf"] {
            assert!(is_device_path(Path::new(device)) && looks_like_pdf(Path::new(device)).is_err(), "{device}");
        }
        assert!(!is_device_path(Path::new("C:\\a.pdf")) && !is_device_path(Path::new("\\\\server\\share\\a.pdf")));
        // Many files are looked at at once, each answered in its place; a missing one is its own error.
        let files = vec![dir.join("a.pdf"), dir.join("b.pdf"), dir.join("missing.pdf"), dir.join("junk.pdf")];
        let got: Vec<Option<bool>> = sniff(&files, Duration::from_secs(10)).into_iter().map(|r| r.ok()).collect();
        assert_eq!(got, vec![Some(true), Some(false), None, Some(true)]);
        assert!(sniff(&[], Duration::from_secs(1)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_look_that_does_not_answer_in_time_is_given_up_on_and_the_others_are_not_held_up() {
        let quick: Look = Box::new(|| Looked::Pdf(Ok(true)));
        let slow: Look = Box::new(|| {
            std::thread::sleep(Duration::from_millis(600));
            Looked::Pdf(Ok(false))
        });
        let started = Instant::now();
        let got = look_all(vec![slow, quick], Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_millis(500), "waited for the slow one");
        assert!(got[0].is_none() && matches!(got[1], Some(Looked::Pdf(Ok(true)))));
        assert!(look_all(Vec::new(), Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_name_is_cut_with_an_ellipsis_to_fit_and_left_alone_when_it_fits() {
        let by_chars = |max: usize| move |s: &str| s.chars().count() <= max;
        assert_eq!(shorten_to("report.pdf", by_chars(10)), "report.pdf");
        assert_eq!(shorten_to("quarterly-report.pdf", by_chars(8)), "quarter\u{2026}");
        assert_eq!(shorten_to("公文公文公文公文", by_chars(4)), "公文公\u{2026}");
        assert_eq!(shorten_to("abc", by_chars(1)), "\u{2026}");
        assert_eq!(shorten_to("abc", by_chars(0)), "");
        assert_eq!(label(Path::new("C:\\docs\\a.pdf")), "a.pdf");
        assert_eq!(label(Path::new("C:\\")), "C:\\");
    }
}
