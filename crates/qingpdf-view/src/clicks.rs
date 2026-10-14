//! Counting clicks: a click that comes soon enough after the one before, near it, is the second of a double click, and so on
//! (a double click selects a word, a triple click a line). The counting is plain; the window gives the time and place.

/// What the last click was.
#[derive(Clone, Copy, Debug)]
struct Last {
    time: u32,
    x: i32,
    y: i32,
    count: u8,
}

#[derive(Debug, Default)]
pub struct ClickCounter {
    last: Option<Last>,
}

impl ClickCounter {
    /// A click at `time` (milliseconds on a clock that may wrap round) at (`x`, `y`): which one in a row it is, 1 to 3 (a fourth
    /// click starts again at 1, as it does in other programs). A click is part of the row when it comes within `max_ms` of the one
    /// before and no more than `half_w` across and `half_h` down from it.
    pub fn click(&mut self, time: u32, x: i32, y: i32, max_ms: u32, half_w: i32, half_h: i32) -> u8 {
        let count = match self.last {
            Some(l) if time.wrapping_sub(l.time) <= max_ms && (x - l.x).abs() <= half_w && (y - l.y).abs() <= half_h => l.count % 3 + 1,
            _ => 1,
        };
        self.last = Some(Last { time, x, y, count });
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_in_a_row_count_up_to_three_and_start_again() {
        let mut c = ClickCounter::default();
        assert_eq!(c.click(1000, 50, 50, 500, 2, 2), 1);
        assert_eq!(c.click(1200, 51, 50, 500, 2, 2), 2);
        assert_eq!(c.click(1400, 50, 49, 500, 2, 2), 3);
        assert_eq!(c.click(1600, 50, 50, 500, 2, 2), 1);
        assert_eq!(c.click(1700, 50, 50, 500, 2, 2), 2);
    }

    #[test]
    fn a_slow_click_or_a_far_one_starts_a_new_row() {
        let mut c = ClickCounter::default();
        assert_eq!(c.click(1000, 50, 50, 500, 2, 2), 1);
        assert_eq!(c.click(1600, 50, 50, 500, 2, 2), 1);
        assert_eq!(c.click(1700, 60, 50, 500, 2, 2), 1);
        assert_eq!(c.click(1800, 60, 60, 500, 2, 2), 1);
        assert_eq!(c.click(1900, 60, 60, 500, 2, 2), 2);
    }

    #[test]
    fn the_clock_may_wrap_round() {
        let mut c = ClickCounter::default();
        assert_eq!(c.click(u32::MAX - 100, 5, 5, 500, 2, 2), 1);
        assert_eq!(c.click(100, 5, 5, 500, 2, 2), 2);
    }
}
