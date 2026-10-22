//! The little messages over the pages (2026-10-21; before that, the line of the status bar): one at a time, each fades in, stays about five and a half seconds and
//! fades out; those that come while one is showing wait their turn, in the order they came. A message that is the same as one showing or waiting but for its
//! numbers ("Compressing 41%", "Compressing 42%") takes its place instead of lining up behind it. Plain logic on a clock in milliseconds; `app/overlay.rs`
//! draws what [`Toasts::view`] says.
//!
//! A second kind, the **sticky** message (2026-10-22), does not go by itself: it is what a job that is running ("Exporting... (Esc to cancel)") or a tool that is in use
//! ("Drag over the text to mark it (Esc ends)") says, and it is shown for as long as the job or the tool lasts (the reader says what it is, again, whenever that
//! changes, and none when it has ended). Ordinary messages come before it and it comes back after them. Long words are laid in two lines ([`wrap`]), not cut.

use std::collections::VecDeque;

pub const FADE_IN: u64 = 160;
pub const HOLD: u64 = 5500;
pub const FADE_OUT: u64 = 240;
/// Most messages that wait; one more makes the oldest of them go.
pub const MAX_WAITING: usize = 8;
/// How far (at 96 dpi) a message rises while it fades in.
pub const LIFT: i32 = 6;
/// The end of a message that does not go by itself.
const STICKY: u64 = u64::MAX;

struct Showing {
    text: String,
    /// When it began to fade in, and when it begins to fade out ([`STICKY`]: when it is told to).
    start: u64,
    end: u64,
    /// It came from [`Toasts::set_sticky`] (and so is not merged with an ordinary message of the same stem).
    from_sticky: bool,
}

impl Showing {
    /// A sticky message that has not been told to go.
    fn held(&self) -> bool {
        self.end == STICKY
    }
}

#[derive(Default)]
pub struct Toasts {
    waiting: VecDeque<String>,
    showing: Option<Showing>,
    /// What the job or the tool that is going on says, and the words of it that were closed by a click (they are not shown again until they are other words).
    sticky: Option<String>,
    closed: Option<String>,
    /// Windows is set to show no animations: a message appears and goes at once.
    pub still: bool,
}

/// What to draw: the words, the opacity (0 to 255) and how many of `LIFT`'s 96ths it is still below its place (0 to `LIFT`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View<'a> {
    pub text: &'a str,
    pub alpha: u8,
    pub lift: i32,
}

/// The text without its numbers and spaces: two messages with the same stem are one message that changes.
fn stem(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii_digit() && !c.is_whitespace() && *c != '%').collect()
}

fn was_closed(closed: &Option<String>, text: &str) -> bool {
    closed.as_deref().is_some_and(|c| stem(c) == stem(text))
}

impl Toasts {
    pub fn new() -> Toasts {
        Toasts::default()
    }

    fn fade_in(&self) -> u64 {
        if self.still { 0 } else { FADE_IN }
    }

    fn fade_out(&self) -> u64 {
        if self.still { 0 } else { FADE_OUT }
    }

    /// A message to show (an empty one is not shown).
    pub fn push(&mut self, text: String, now: u64) {
        if text.trim().is_empty() {
            return;
        }
        let key = stem(&text);
        // The same message as the last one that waits: it is that one, changed.
        if let Some(last) = self.waiting.back_mut().filter(|w| stem(w) == key) {
            *last = text;
            return;
        }
        // The same as the one showing (and nothing waits): the words change, and it stays for as long as a new one would.
        let fade_in = self.fade_in();
        if self.waiting.is_empty()
            && let Some(cur) = self.showing.as_mut().filter(|c| !c.from_sticky && stem(&c.text) == key)
        {
            let going = now >= cur.end;
            cur.text = text;
            cur.end = now + HOLD;
            if going {
                // It had begun to go: it is back at once, with no fade-in.
                cur.start = now.saturating_sub(fade_in);
            }
            return;
        }
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back(text);
        self.advance(now);
    }

    /// What the job or the tool that is going on says now (none: nothing is going on). It is shown whenever no other message is, and goes when it is none.
    pub fn set_sticky(&mut self, text: Option<String>, now: u64) {
        if text.is_none() {
            self.closed = None;
        }
        self.sticky = text.filter(|t| !t.trim().is_empty());
        self.advance(now);
    }

    pub fn sticky(&self) -> Option<&str> {
        self.sticky.as_deref()
    }

    /// The next message goes on when the one showing has gone (call it as time passes); a sticky message gives way to a message that waits and goes when it is
    /// wanted no more, and one that is wanted and has nothing before it comes.
    pub fn advance(&mut self, now: u64) {
        let (fade_in, fade_out) = (self.fade_in(), self.fade_out());
        let Toasts { waiting, showing, sticky, closed, .. } = self;
        if let Some(c) = showing.as_mut().filter(|c| c.held()) {
            match sticky {
                Some(words) if waiting.is_empty() => c.text.clone_from(words),
                // Not wanted any more, or a message is waiting for its turn: it begins to go.
                _ => c.end = now,
            }
        }
        if showing.as_ref().is_some_and(|c| !c.held() && now >= c.end.saturating_add(fade_out)) {
            *showing = None;
        }
        if showing.is_none() {
            if let Some(text) = waiting.pop_front() {
                *showing = Some(Showing { text, start: now, end: now + fade_in + HOLD, from_sticky: false });
            } else if let Some(text) = sticky.clone().filter(|t| !was_closed(closed, t)) {
                *showing = Some(Showing { text, start: now, end: STICKY, from_sticky: true });
            }
        }
    }

    /// What to draw at `now`.
    pub fn view(&self, now: u64) -> Option<View<'_>> {
        let c = self.showing.as_ref()?;
        let since = now.saturating_sub(c.start);
        let (fade_in, fade_out) = (self.fade_in(), self.fade_out());
        let (alpha, lift) = if since < fade_in {
            let k = since * 255 / fade_in;
            (k as u8, (i64::from(LIFT) * (255 - k as i64) / 255) as i32)
        } else if now < c.end {
            (255, 0)
        } else if now < c.end.saturating_add(fade_out) {
            (255 - ((now - c.end) * 255 / fade_out) as u8, 0)
        } else {
            return None;
        };
        Some(View { text: &c.text, alpha, lift })
    }

    /// Esc: the ordinary message showing goes now (it fades out; the next one comes after it). `false` when none is showing, it is going already, or it is a
    /// sticky one (that goes when its job or its tool ends: Esc is for the job, the tool, the find box).
    pub fn dismiss(&mut self, now: u64) -> bool {
        match self.showing.as_mut() {
            Some(c) if !c.held() && now < c.end => {
                c.end = now;
                true
            }
            _ => false,
        }
    }

    /// A click on the message: it goes now, a sticky one too (and is not shown again until it says something else).
    pub fn close(&mut self, now: u64) -> bool {
        let Some(c) = self.showing.as_mut() else { return false };
        if now >= c.end {
            return false;
        }
        if c.held() {
            self.closed = Some(c.text.clone());
        }
        c.end = now;
        true
    }

    /// Is a message showing and not going (Esc, or a click, would close it)?
    pub fn is_showing(&self, now: u64) -> bool {
        self.showing.as_ref().is_some_and(|c| now < c.end)
    }

    /// Is a message showing, or are there some waiting, or is a sticky one to come?
    pub fn busy(&self) -> bool {
        self.showing.is_some() || !self.waiting.is_empty() || self.sticky.as_deref().is_some_and(|s| !was_closed(&self.closed, s))
    }

    /// Does the picture of the messages change as time passes (so that the clock must go on)? Not while a message stands in full: a sticky one for as long as it is
    /// wanted, an ordinary one for as long as it is held (`next_change` says when that ends).
    pub fn moving(&self, now: u64) -> bool {
        match &self.showing {
            Some(c) if now < c.start.saturating_add(self.fade_in()) => true,
            Some(c) if c.held() => self.sticky.is_none() || !self.waiting.is_empty(),
            Some(c) => now >= c.end,
            None => !self.waiting.is_empty() || self.sticky.as_deref().is_some_and(|s| !was_closed(&self.closed, s)),
        }
    }

    /// How long until the picture changes again when it is only being held (milliseconds from `now`), if it is.
    pub fn next_change(&self, now: u64) -> Option<u64> {
        let c = self.showing.as_ref()?;
        (!c.held() && now >= c.start.saturating_add(self.fade_in()) && now < c.end).then(|| c.end - now)
    }

    pub fn clear(&mut self) {
        *self = Toasts { still: self.still, ..Toasts::default() };
    }
}

/// Lay `text` in lines of at most `max` pixels (`width` measures a piece of it), at most `lines` of them. Lines break after a space or after a character that is not
/// ASCII (a Chinese sentence has no spaces); a word wider than a line is broken where it has to be. When the text needs more lines than that, the last line is
/// the rest of the text cut in the middle with an ellipsis, so that the end of it (the "Esc ends" of a hint) stays.
pub fn wrap(text: &str, max: i32, lines: usize, width: &mut dyn FnMut(&str) -> i32) -> Vec<String> {
    let lines = lines.max(1);
    let mut pieces: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        word.push(c);
        if c == ' ' || !c.is_ascii() {
            pieces.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        pieces.push(word);
    }
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while at < pieces.len() && out.len() < lines {
        if out.len() + 1 == lines {
            // The last line: all the rest, cut in the middle if it is too wide.
            let rest: String = pieces.get(at..).unwrap_or_default().concat();
            let rest = rest.trim_start().to_string();
            out.push(cut_middle(&rest, max, width));
            return out;
        }
        let mut line = String::new();
        while let Some(piece) = pieces.get(at) {
            let joined = format!("{line}{piece}");
            if line.is_empty() || width(joined.trim_end()) <= max {
                line = joined;
                at += 1;
            } else {
                break;
            }
        }
        // A single piece wider than the line is broken where it has to be.
        if width(line.trim_end()) > max {
            let chars: Vec<char> = line.trim_end().chars().collect();
            let mut keep = chars.len();
            while keep > 1 && width(&chars.iter().take(keep).collect::<String>()) > max {
                keep -= 1;
            }
            let (head, tail): (String, String) = (chars.iter().take(keep).collect(), chars.iter().skip(keep).collect());
            out.push(head);
            if !tail.is_empty()
                && at > 0
                && let Some(slot) = pieces.get_mut(at - 1)
            {
                *slot = tail;
                at -= 1;
            }
            continue;
        }
        out.push(line.trim_end().to_string());
    }
    out
}

/// `s` as it is when it fits `max`; else with its middle replaced by an ellipsis so that it does.
fn cut_middle(s: &str, max: i32, width: &mut dyn FnMut(&str) -> i32) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let cut = |keep: usize| -> String {
        let (head, tail) = (keep / 3, keep - keep / 3);
        chars.iter().take(head).chain(std::iter::once(&'\u{2026}')).chain(chars.iter().skip(chars.len().saturating_sub(tail))).collect()
    };
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(t: &Toasts, now: u64) -> Option<(String, u8)> {
        t.view(now).map(|v| (v.text.to_string(), v.alpha))
    }

    /// Seven pixels to a character.
    fn seven(s: &str) -> i32 {
        7 * s.chars().count() as i32
    }

    #[test]
    fn a_message_fades_in_stays_and_fades_out() {
        let mut t = Toasts::new();
        t.push("Copied 12 characters".to_string(), 1000);
        assert_eq!(words(&t, 1000), Some(("Copied 12 characters".to_string(), 0)));
        let half = t.view(1000 + FADE_IN / 2).unwrap();
        assert!(half.alpha > 100 && half.alpha < 160 && half.lift > 0 && half.lift < LIFT, "{half:?}");
        assert_eq!(t.view(1000 + FADE_IN).unwrap(), View { text: "Copied 12 characters", alpha: 255, lift: 0 });
        assert_eq!(t.view(1000 + FADE_IN + HOLD - 1).unwrap().alpha, 255);
        let out = t.view(1000 + FADE_IN + HOLD + FADE_OUT / 2).unwrap().alpha;
        assert!(out > 100 && out < 160, "{out}");
        assert_eq!(t.view(1000 + FADE_IN + HOLD + FADE_OUT), None);
        t.advance(1000 + FADE_IN + HOLD + FADE_OUT);
        assert!(!t.busy());
    }

    #[test]
    fn an_ordinary_message_stays_five_and_a_half_seconds_the_way_the_old_status_note_did() {
        assert!((5000..=6000).contains(&HOLD));
        let mut t = Toasts::new();
        t.push("Saved.".to_string(), 0);
        assert_eq!(t.view(FADE_IN + 5000).map(|v| v.alpha), Some(255));
        assert!(t.view(FADE_IN + HOLD + FADE_OUT).is_none());
    }

    #[test]
    fn messages_come_one_at_a_time_in_the_order_they_were_said() {
        let mut t = Toasts::new();
        t.push("first".to_string(), 0);
        t.push("second".to_string(), 10);
        t.push("third".to_string(), 20);
        assert_eq!(words(&t, 500).unwrap().0, "first");
        // Nothing of the second shows while the first does; when the first has gone the second comes.
        let gone = FADE_IN + HOLD + FADE_OUT;
        assert_eq!(words(&t, gone - 1).unwrap().0, "first");
        t.advance(gone);
        assert_eq!(t.view(gone).unwrap().text, "second");
        assert_eq!(t.view(gone).unwrap().alpha, 0, "it fades in too");
        t.advance(gone + FADE_IN + HOLD + FADE_OUT);
        assert_eq!(t.view(gone + FADE_IN + HOLD + FADE_OUT).unwrap().text, "third");
        assert!(t.busy());
    }

    #[test]
    fn esc_closes_the_one_showing_and_the_next_comes_after_it() {
        let mut t = Toasts::new();
        t.push("one".to_string(), 0);
        t.push("two".to_string(), 0);
        assert!(t.is_showing(1000));
        assert!(t.dismiss(1000));
        assert!(!t.is_showing(1000) && !t.dismiss(1000), "going already");
        // It fades out from where it is (a quarter of a second), and then the next one comes.
        assert!(t.view(1000).unwrap().alpha > 200);
        assert_eq!(t.view(1000 + FADE_OUT), None);
        t.advance(1000 + FADE_OUT);
        assert_eq!(t.view(1000 + FADE_OUT).unwrap().text, "two");
        let mut none = Toasts::new();
        assert!(!none.dismiss(0));
    }

    #[test]
    fn a_message_that_only_differs_in_its_numbers_takes_the_place_of_the_one_showing_or_waiting() {
        let mut t = Toasts::new();
        t.push("Compressing 10%".to_string(), 0);
        t.push("Compressing 11%".to_string(), 500);
        t.push("Compressing 12%".to_string(), 900);
        assert_eq!(t.view(1000).unwrap().text, "Compressing 12%");
        // It stays as long as a new one would from the last change.
        assert_eq!(t.view(900 + HOLD - 10).unwrap().alpha, 255);
        // Others wait; a later change of the same kind changes the one that waits, not a new one.
        t.push("Done".to_string(), 1000);
        t.push("Done 1".to_string(), 1001);
        t.push("Done 2".to_string(), 1002);
        let gone = 900 + HOLD + FADE_OUT;
        t.advance(gone);
        assert_eq!(t.view(gone).unwrap().text, "Done 2");
        t.advance(gone + FADE_IN + HOLD + FADE_OUT);
        assert!(t.view(gone + FADE_IN + HOLD + FADE_OUT).is_none() && !t.busy(), "only one Done");
        // A message that had begun to go comes back without fading in again.
        let mut back = Toasts::new();
        back.push("Saved 1".to_string(), 0);
        let late = FADE_IN + HOLD + 100;
        assert!(back.view(late).unwrap().alpha < 255);
        back.push("Saved 2".to_string(), late);
        assert_eq!(back.view(late).unwrap(), View { text: "Saved 2", alpha: 255, lift: 0 });
    }

    #[test]
    fn empty_messages_are_not_shown_and_not_many_wait() {
        let mut t = Toasts::new();
        t.push(String::new(), 0);
        t.push("   ".to_string(), 0);
        assert!(!t.busy());
        t.push("a".to_string(), 0);
        for i in 0..40 {
            t.push(format!("message {}", "x".repeat(i + 1)), 0);
        }
        assert!(t.waiting.len() <= MAX_WAITING);
        // The oldest waiting ones were let go, the newest is kept.
        assert!(t.waiting.back().is_some_and(|w| w.len() == "message ".len() + 40));
        t.clear();
        assert!(!t.busy() && t.view(0).is_none());
    }

    #[test]
    fn a_clock_that_went_back_does_not_break_anything() {
        let mut t = Toasts::new();
        t.push("x".to_string(), 5000);
        assert_eq!(t.view(100).unwrap().alpha, 0);
        t.advance(100);
        assert!(t.busy());
    }

    #[test]
    fn a_sticky_message_stays_for_as_long_as_it_is_wanted_and_goes_when_it_is_not() {
        let mut t = Toasts::new();
        t.set_sticky(Some("Exporting... (Esc to cancel)".to_string()), 0);
        assert_eq!(t.view(0).map(|v| v.alpha), Some(0), "it fades in");
        assert_eq!(t.view(FADE_IN).unwrap(), View { text: "Exporting... (Esc to cancel)", alpha: 255, lift: 0 });
        // A minute later it is still there, at full strength, and nothing needs the clock for it.
        for now in [10_000, 60_000, 3_600_000] {
            t.advance(now);
            assert_eq!(t.view(now).map(|v| v.alpha), Some(255), "{now}");
            assert!(!t.moving(now) && t.next_change(now).is_none() && t.is_showing(now));
        }
        // What it says changes in place (no fade again).
        t.set_sticky(Some("Exporting... 40% (Esc to cancel)".to_string()), 70_000);
        assert_eq!(t.view(70_000).unwrap(), View { text: "Exporting... 40% (Esc to cancel)", alpha: 255, lift: 0 });
        // When it is wanted no more it fades out and is gone.
        t.set_sticky(None, 80_000);
        assert!(t.moving(80_000));
        assert!(t.view(80_000 + FADE_OUT / 2).unwrap().alpha < 255);
        t.advance(80_000 + FADE_OUT);
        assert!(t.view(80_000 + FADE_OUT).is_none() && !t.busy() && !t.moving(80_000 + FADE_OUT));
    }

    #[test]
    fn an_ordinary_message_comes_before_a_sticky_one_and_the_sticky_one_comes_back_after_it() {
        let mut t = Toasts::new();
        t.set_sticky(Some("Drag over the text (Esc ends)".to_string()), 0);
        t.advance(1000);
        t.push("Copied.".to_string(), 1000);
        // The sticky one begins to go; when it has gone the message shows.
        assert!(t.view(1000 + FADE_OUT / 2).is_some_and(|v| v.text.starts_with("Drag")));
        t.advance(1000 + FADE_OUT);
        assert_eq!(t.view(1000 + FADE_OUT).unwrap().text, "Copied.");
        let over = 1000 + FADE_OUT + FADE_IN + HOLD + FADE_OUT;
        t.advance(over);
        assert_eq!(t.view(over).unwrap().text, "Drag over the text (Esc ends)");
        t.advance(over + FADE_IN);
        assert_eq!(t.view(over + FADE_IN).unwrap().alpha, 255);
    }

    #[test]
    fn esc_does_not_close_a_sticky_message_but_a_click_does_until_it_says_something_else() {
        let mut t = Toasts::new();
        t.set_sticky(Some("Compressing... 10% (Esc cancels)".to_string()), 0);
        t.advance(500);
        assert!(!t.dismiss(500), "Esc is for the job");
        assert!(t.is_showing(500));
        assert!(t.close(500));
        t.advance(500 + FADE_OUT);
        assert!(t.view(500 + FADE_OUT).is_none());
        // The numbers changing is not saying something else; other words are.
        t.set_sticky(Some("Compressing... 11% (Esc cancels)".to_string()), 1000);
        t.advance(1000);
        assert!(t.view(1100).is_none(), "still closed");
        t.set_sticky(Some("Saving...".to_string()), 1200);
        t.advance(1200);
        assert_eq!(t.view(1200).unwrap().text, "Saving...");
        // Once the job is over the closing is forgotten.
        t.set_sticky(None, 1300);
        t.advance(1300 + FADE_OUT);
        t.set_sticky(Some("Compressing... 5% (Esc cancels)".to_string()), 2000);
        t.advance(2000);
        assert_eq!(t.view(2000).unwrap().text, "Compressing... 5% (Esc cancels)");
    }

    #[test]
    fn a_held_message_asks_for_the_clock_only_when_it_has_to_change() {
        let mut t = Toasts::new();
        t.push("Saved.".to_string(), 0);
        assert!(t.moving(0) && t.next_change(0).is_none(), "fading in");
        assert!(!t.moving(FADE_IN + 100));
        assert_eq!(t.next_change(FADE_IN + 100), Some(HOLD - 100));
        assert!(t.moving(FADE_IN + HOLD + 10) && t.next_change(FADE_IN + HOLD + 10).is_none(), "fading out");
    }

    #[test]
    fn with_no_animations_a_message_is_there_at_once_and_goes_at_once() {
        let mut t = Toasts::new();
        t.still = true;
        t.push("Saved.".to_string(), 100);
        assert_eq!(t.view(100).unwrap(), View { text: "Saved.", alpha: 255, lift: 0 });
        assert!(t.view(100 + HOLD - 1).is_some() && t.view(100 + FADE_IN + HOLD).is_none());
        t.set_sticky(Some("Job".to_string()), 20_000);
        assert_eq!(t.view(20_000).map(|v| v.alpha), Some(255));
        t.set_sticky(None, 30_000);
        t.advance(30_000);
        assert!(t.view(30_000).is_none());
    }

    #[test]
    fn long_words_are_laid_in_two_lines_and_the_end_of_a_hint_stays() {
        let hint = "Drag a box for the text (or click), type, then click outside or press Ctrl+Enter. Esc ends.";
        // Room for the whole in one line: one line.
        assert_eq!(wrap(hint, 700, 2, &mut |s| seven(s)), vec![hint.to_string()]);
        // Not enough for one line: two, broken after a space, nothing lost.
        let two = wrap(hint, 400, 2, &mut |s| seven(s));
        assert_eq!(two.len(), 2, "{two:?}");
        assert!(two.iter().all(|l| seven(l) <= 400));
        assert_eq!(two.join(" "), hint);
        assert!(two[1].ends_with("Esc ends."));
        // Not enough for two: the last line is cut in the middle and keeps the end.
        let squeezed = wrap(hint, 250, 2, &mut |s| seven(s));
        assert_eq!(squeezed.len(), 2);
        assert!(squeezed.iter().all(|l| seven(l) <= 250), "{squeezed:?}");
        assert!(squeezed[1].contains('\u{2026}') && squeezed[1].ends_with("Esc ends."), "{squeezed:?}");
        assert!(squeezed[0].starts_with("Drag a box"));
    }

    #[test]
    fn chinese_has_no_spaces_and_is_broken_between_characters() {
        let hint = "拖出放文字的框（或点一下），输入，然后点框外或按 Ctrl+Enter。按 Esc 结束。";
        let width = |s: &str| s.chars().map(|c| if c.is_ascii() { 7 } else { 13 }).sum::<i32>();
        let lines = wrap(hint, 220, 2, &mut |s| width(s));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|l| width(l) <= 220), "{lines:?}");
        assert!(lines[1].ends_with("按 Esc 结束。"), "{lines:?}");
        // A word wider than the line is broken where it has to be.
        let broken = wrap("supercalifragilisticexpialidocious", 70, 3, &mut |s| seven(s));
        assert_eq!(broken.len(), 3);
        assert!(broken.iter().all(|l| seven(l) <= 70));
        assert_eq!(wrap("", 100, 2, &mut |s| seven(s)), Vec::<String>::new());
    }
}
