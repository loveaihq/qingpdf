//! The little messages over the pages (2026-10-21; before that, the line of the status bar): one at a time, each fades in, stays about three seconds and fades
//! out; those that come while one is showing wait their turn, in the order they came. A message that is the same as one showing or waiting but for its
//! numbers ("Compressing 41%", "Compressing 42%") takes its place instead of lining up behind it. Plain logic on a clock in milliseconds; `app/overlay.rs`
//! draws what [`Toasts::view`] says.

use std::collections::VecDeque;

pub const FADE_IN: u64 = 160;
pub const HOLD: u64 = 3000;
pub const FADE_OUT: u64 = 240;
/// Most messages that wait; one more makes the oldest of them go.
pub const MAX_WAITING: usize = 8;
/// How far (at 96 dpi) a message rises while it fades in.
pub const LIFT: i32 = 6;

struct Showing {
    text: String,
    /// When it began to fade in, and when it begins to fade out.
    start: u64,
    end: u64,
}

#[derive(Default)]
pub struct Toasts {
    waiting: VecDeque<String>,
    showing: Option<Showing>,
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

impl Toasts {
    pub fn new() -> Toasts {
        Toasts::default()
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
        if self.waiting.is_empty()
            && let Some(cur) = self.showing.as_mut().filter(|c| stem(&c.text) == key)
        {
            let fading_out = now >= cur.end;
            cur.text = text;
            cur.end = now + HOLD;
            if fading_out {
                // It had begun to go: it is back at once, with no fade-in.
                cur.start = now.saturating_sub(FADE_IN);
            }
            return;
        }
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back(text);
        self.advance(now);
    }

    /// The next message goes on when the one showing has gone (call it as time passes).
    pub fn advance(&mut self, now: u64) {
        if self.showing.as_ref().is_some_and(|c| now >= c.end + FADE_OUT) {
            self.showing = None;
        }
        if self.showing.is_none()
            && let Some(text) = self.waiting.pop_front()
        {
            self.showing = Some(Showing { text, start: now, end: now + FADE_IN + HOLD });
        }
    }

    /// What to draw at `now`.
    pub fn view(&self, now: u64) -> Option<View<'_>> {
        let c = self.showing.as_ref()?;
        let since = now.saturating_sub(c.start);
        let (alpha, lift) = if since < FADE_IN {
            let k = since * 255 / FADE_IN;
            (k as u8, (i64::from(LIFT) * (255 - k as i64) / 255) as i32)
        } else if now < c.end {
            (255, 0)
        } else if now < c.end + FADE_OUT {
            (255 - ((now - c.end) * 255 / FADE_OUT) as u8, 0)
        } else {
            return None;
        };
        Some(View { text: &c.text, alpha, lift })
    }

    /// Esc: the message showing goes now (it fades out; the next one comes after it). `false` when none is showing or it is going already.
    pub fn dismiss(&mut self, now: u64) -> bool {
        match self.showing.as_mut() {
            Some(c) if now < c.end => {
                c.end = now;
                true
            }
            _ => false,
        }
    }

    /// Is a message showing and not going (Esc would close it)?
    pub fn is_showing(&self, now: u64) -> bool {
        self.showing.as_ref().is_some_and(|c| now < c.end)
    }

    /// Is there anything showing or waiting (so that the clock must go on)?
    pub fn busy(&self) -> bool {
        self.showing.is_some() || !self.waiting.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Toasts::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(t: &Toasts, now: u64) -> Option<(String, u8)> {
        t.view(now).map(|v| (v.text.to_string(), v.alpha))
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
        // It stays three seconds from the last change.
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
}
