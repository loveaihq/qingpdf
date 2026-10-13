//! What a cell's text is in a spreadsheet: a number, a date, or text (ISO/IEC 29500-1, 18.8 number formats, 18.17.4 dates).
//!
//! A number is only made of text that is surely one: digits with the thousands separators put right (`1,234.50`), a sign or
//! brackets for a negative (`-12`, `(12)`), a percent, a currency sign. Brackets or a minus after the number make a whole number
//! negative only in a column of amounts ([`Value::Weak`]; `(1)` and `3-` are often list numbers). Anything a spreadsheet would spoil stays text: an identity
//! number or a phone number (a plain whole number of ten digits or more), a code that begins with a zero (`007`), a number of more
//! than 15 digits (a spreadsheet keeps 15), a version (`1.2.3`), a range (`3-5`).

use std::fmt::Write as _;

/// How a number is shown. The default is the general format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Fmt {
    /// 0 none, 1 yuan (¥), 2 dollar, 3 euro, 4 pound.
    cur: u8,
    decimals: u8,
    thousands: bool,
    /// A negative number is shown in brackets.
    paren: bool,
    percent: bool,
    /// A date: 0 no, 1 `-`, 2 `/`, 3 `.`, 4 year, month and day in Chinese. `pad`: two digits for the month and the day.
    date: u8,
    pad: bool,
    /// With a time: 1 hours and minutes, 2 and seconds.
    time: u8,
}

impl Fmt {
    pub(super) fn is_date(&self) -> bool {
        self.date != 0
    }

    /// Written as an amount: with decimals, thousands separators, a currency sign or a percent.
    pub(super) fn rich(&self) -> bool {
        self.decimals > 0 || self.thousands || self.cur != 0 || self.percent
    }
}

impl Fmt {
    /// The number format code (18.8.31).
    pub(super) fn code(&self) -> String {
        let mut s = String::new();
        if self.date != 0 {
            let (m, d) = if self.pad { ("mm", "dd") } else { ("m", "d") };
            let _ = match self.date {
                1 => write!(s, "yyyy-{m}-{d}"),
                2 => write!(s, "yyyy/{m}/{d}"),
                3 => write!(s, "yyyy.{m}.{d}"),
                _ => write!(s, "yyyy\"年\"{m}\"月\"{d}\"日\""),
            };
            match self.time {
                1 => s.push_str(" hh:mm"),
                2 => s.push_str(" hh:mm:ss"),
                _ => {}
            }
            return s;
        }
        let mut one = String::new();
        if let Some(sign) = [None, Some('¥'), Some('$'), Some('€'), Some('£')].get(usize::from(self.cur)).copied().flatten() {
            let _ = write!(one, "\"{sign}\"");
        }
        one.push_str(if self.thousands { "#,##0" } else { "0" });
        if self.decimals > 0 {
            one.push('.');
            one.extend(std::iter::repeat_n('0', usize::from(self.decimals)));
        }
        if self.percent {
            one.push('%');
        }
        if self.paren {
            let _ = write!(s, "{one}_);({one})");
        } else {
            s = one;
        }
        s
    }
}

/// What a text is.
#[derive(Debug, PartialEq)]
pub(super) enum Value {
    Text,
    Number(f64, Fmt),
    /// A whole number in brackets or with the minus after it (`(1)`, `3-`): a negative number in a column of amounts, else a
    /// list number, a heading, a range, and so text. The sheet decides (the column is looked at).
    Weak(f64, Fmt),
}

/// Most characters of a text looked at as a number or a date (a longer one is text).
const MAX_NUMBER_TEXT: usize = 40;

pub(super) fn detect(text: &str) -> Value {
    let t = text.trim();
    if t.is_empty() || t.len() > MAX_NUMBER_TEXT {
        return Value::Text;
    }
    match date(t).map(|(v, f)| (v, f, false)).or_else(|| number(t)) {
        Some((v, f, true)) => Value::Weak(v, f),
        Some((v, f, false)) => Value::Number(v, f),
        None => Value::Text,
    }
}

/// `s` without its first character if that is one of the signs; and whether it was a minus.
fn sign(s: &str) -> Option<(&str, bool)> {
    let c = s.chars().next()?;
    let rest = s.get(c.len_utf8()..)?;
    match c {
        '-' | '−' | '－' => Some((rest, true)),
        '+' | '＋' => Some((rest, false)),
        _ => None,
    }
}

/// The number, how it is shown, and whether it is a weak negative (see [`Value::Weak`]).
fn number(t: &str) -> Option<(f64, Fmt, bool)> {
    let mut f = Fmt::default();
    let (mut s, mut negative, mut signed, mut after) = (t, false, false, false);
    if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        (s, negative, f.paren) = (inner, true, true);
    }
    if let Some((rest, minus)) = sign(s) {
        // (A minus inside brackets is two ways of saying it.)
        if f.paren {
            return None;
        }
        (s, negative, signed) = (rest.trim_start(), minus, true);
    }
    if let Some(i) = ["¥", "￥", "$", "€", "£"].iter().position(|c| s.starts_with(c)) {
        f.cur = [1, 1, 2, 3, 4].get(i).copied()?;
        s = s.get(["¥", "￥", "$", "€", "£"].get(i)?.len()..)?.trim_start();
        if let Some((rest, minus)) = sign(s) {
            if signed || f.paren {
                return None;
            }
            (s, negative, signed) = (rest, minus, true);
        }
    }
    if let Some(rest) = s.strip_suffix('%') {
        (s, f.percent) = (rest, true);
    } else if !signed && !f.paren && s.len() > 1 && s.ends_with(['-', '−']) {
        // (An amount with the minus after it.)
        s = s.get(..s.len() - s.chars().next_back()?.len_utf8())?;
        (negative, after) = (true, true);
    }
    let (whole, fraction) = match s.split_once('.') {
        Some((w, d)) => (w, Some(d)),
        None => (s, None),
    };
    f.thousands = whole.contains(',');
    let mut digits = 0usize;
    if f.thousands {
        // 1 to 3 digits, then groups of three, and no zero to begin with (`0,123` is no number).
        let mut groups = whole.split(',');
        let first = groups.next()?;
        if first.is_empty() || first.len() > 3 || first.starts_with('0') || !first.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits += first.len();
        for g in groups {
            if g.len() != 3 || !g.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits += 3;
        }
    } else {
        // A zero first is a code, unless the number is that zero (or `0.5`).
        if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || (whole.len() > 1 && whole.starts_with('0')) {
            return None;
        }
        digits += whole.len();
    }
    if let Some(d) = fraction {
        if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits += d.len();
        f.decimals = u8::try_from(d.len().min(10)).ok()?;
    }
    // A spreadsheet keeps 15 digits; a whole number of ten digits is an identity number, a phone number, a card.
    if digits > 15 || (!f.thousands && fraction.is_none() && digits >= 10) {
        return None;
    }
    let mut clean = String::with_capacity(digits + 2);
    clean.extend(whole.chars().filter(|c| *c != ','));
    if let Some(d) = fraction {
        clean.push('.');
        clean.push_str(d);
    }
    let mut v: f64 = clean.parse().ok()?;
    if f.percent {
        v /= 100.0;
    }
    if negative && v != 0.0 {
        v = -v;
    }
    let weak = (f.paren || after) && !f.rich();
    Some((v, f, weak))
}

fn lstrip(s: &str) -> &str {
    s.trim_start_matches(char::is_whitespace)
}

/// `s` without the spaces at its beginning, if `chinese` (the date is written with 年, 月, 日).
fn blank(s: &str, chinese: bool) -> &str {
    if chinese { lstrip(s) } else { s }
}

/// `s` begins with `min` to `max` digits: their value and how many, and `s` without them.
fn digits(s: &mut &str, min: usize, max: usize) -> Option<(u32, usize)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count().min(max);
    if n < min {
        return None;
    }
    let v = s.get(..n)?.parse().ok()?;
    *s = s.get(n..)?;
    Some((v, n))
}

/// Days from 1970-01-01 to a day of the proleptic Gregorian calendar (the algorithm of Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2024-05-01`, `2024/5/1`, `2024.05.01`, `2024年5月1日` (spaces are allowed round 年, 月 and 日), each with or without a time
/// (`10:30`, `10:30:15`) after it: the day as a spreadsheet counts them (days from 1899-12-30, the time a part of a day), and how to show it. Days before 1900-03-01 stay text
/// (the 1900 of a spreadsheet has a day that was never there).
fn date(t: &str) -> Option<(f64, Fmt)> {
    let mut s = t;
    let (year, _) = digits(&mut s, 4, 4)?;
    // (Spaces are allowed only round the Chinese characters: `2024 年 4 月 19 日`.)
    let chinese = lstrip(s).starts_with('年');
    s = blank(s, chinese);
    let (first, second, kind) = match s.chars().next()? {
        '年' => ('年', '月', 4u8),
        '-' => ('-', '-', 1),
        '/' => ('/', '/', 2),
        '.' => ('.', '.', 3),
        _ => return None,
    };
    s = s.strip_prefix(first)?;
    s = blank(s, chinese);
    let (month, md) = digits(&mut s, 1, 2)?;
    s = blank(s, chinese);
    s = s.strip_prefix(second)?;
    s = blank(s, chinese);
    let (day, dd) = digits(&mut s, 1, 2)?;
    if kind == 4 {
        s = blank(s, chinese);
        s = s.strip_prefix('日')?;
    }
    let long = matches!(month, 1 | 3 | 5 | 7 | 8 | 10 | 12);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let last = if long { 31 } else if month == 2 { 28 + u32::from(leap) } else { 30 };
    if !(1900..=9999).contains(&year) || !(1..=12).contains(&month) || day < 1 || day > last {
        return None;
    }
    let serial = days_from_civil(i64::from(year), i64::from(month), i64::from(day)) + 25_569;
    if serial < 61 {
        return None;
    }
    let (mut time, mut part) = (0u8, 0.0f64);
    if !s.is_empty() {
        s = s.strip_prefix([' ', 'T'])?;
        let (h, _) = digits(&mut s, 1, 2)?;
        s = s.strip_prefix(':')?;
        let (m, _) = digits(&mut s, 2, 2)?;
        let sec = if let Some(rest) = s.strip_prefix(':') {
            s = rest;
            time = 2;
            digits(&mut s, 2, 2)?.0
        } else {
            time = 1;
            0
        };
        if !s.is_empty() || h > 23 || m > 59 || sec > 59 {
            return None;
        }
        part = f64::from(h * 3600 + m * 60 + sec) / 86_400.0;
    }
    Some((serial as f64 + part, Fmt { date: kind, pad: md == 2 || dd == 2, time, ..Fmt::default() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number_of(t: &str) -> Option<(f64, Fmt)> {
        match detect(t) {
            Value::Number(v, f) | Value::Weak(v, f) => Some((v, f)),
            Value::Text => None,
        }
    }

    #[test]
    fn amounts_are_numbers_with_the_format_they_were_written_in() {
        assert_eq!(number_of("12"), Some((12.0, Fmt::default())));
        assert_eq!(number_of("-12"), Some((-12.0, Fmt::default())));
        assert_eq!(number_of("0"), Some((0.0, Fmt::default())));
        let (v, f) = number_of("1,234.50").unwrap();
        assert_eq!((v, f.code().as_str()), (1234.5, "#,##0.00"));
        let (v, f) = number_of("(12.00)").unwrap();
        assert_eq!((v, f.code().as_str()), (-12.0, "0.00_);(0.00)"));
        assert_eq!(number_of("+15,000.00").unwrap().0, 15000.0);
        assert_eq!(number_of("1,234.00-").unwrap().0, -1234.0);
        assert!(matches!(detect("(1,234)"), Value::Number(..)) && matches!(detect("($5)"), Value::Number(..)));
        let (v, f) = number_of("12.5%").unwrap();
        assert_eq!((v, f.code().as_str()), (0.125, "0.0%"));
        let (v, f) = number_of("¥1,234.50").unwrap();
        assert_eq!((v, f.code().as_str()), (1234.5, "\"¥\"#,##0.00"));
        assert_eq!(number_of("-$12").unwrap().0, -12.0);
        assert_eq!(number_of("$-12").unwrap().0, -12.0);
        assert_eq!(number_of("￥ 99").unwrap().0, 99.0);
        assert_eq!(number_of("0.05").unwrap().0, 0.05);
        assert_eq!(number_of("  7 ").unwrap().0, 7.0);
        assert_eq!(number_of("-0").unwrap().0.to_bits(), 0.0f64.to_bits());
    }

    #[test]
    fn what_a_spreadsheet_would_spoil_stays_text() {
        for t in [
            "310101199001011234",
            "13812345678",
            "6222021234567890123",
            "1234567890",
            "007",
            "0123",
            "00.5",
            "-007",
            "1234567890123456.5",
            "1,23",
            "1,2345",
            "0,123",
            "12,345,6",
            "1.2.3",
            "3-5",
            "1e5",
            ".5",
            "5.",
            "1,234.5.6",
            "--5",
            "(-5)",
            "-",
            "—",
            "N/A",
            "12abc",
            "第3页",
            "2024-05",
            "2024-13-01",
            "2024-02-30",
            "1899-12-30",
            "1900-02-28",
            "12:30",
            "",
            "   ",
            "１２３",
        ] {
            assert_eq!(detect(t), Value::Text, "{t:?}");
        }
        // Fifteen digits are fine, sixteen are not; long whole numbers with separators are amounts.
        assert!(number_of("12345678901234.5").is_some() && number_of("123456789012345.5").is_none());
        assert_eq!(number_of("12,345,678,901").unwrap().0, 12_345_678_901.0);
        assert!(number_of("1,234,567,890,123,456").is_none());
    }

    #[test]
    fn a_whole_number_in_brackets_or_with_a_minus_after_it_is_weak() {
        // `(1)` and `3-` are list numbers and ranges as often as they are amounts: the sheet decides by the column.
        for t in ["(1)", "(12)", "3-", "10-", " (4) "] {
            assert!(matches!(detect(t), Value::Weak(..)), "{t:?}");
        }
        assert_eq!(detect("(12)"), Value::Weak(-12.0, number_of("(12)").unwrap().1));
        assert_eq!(number_of("(12)").unwrap().1.code(), "0_);(0)");
        // Sure: a sign in front, decimals, separators, a currency sign, a percent.
        for t in ["-3", "(1.5)", "(1,000)", "3.0-", "($3)", "(3%)", "¥3-"] {
            assert!(matches!(detect(t), Value::Number(..)), "{t:?}");
        }
    }

    #[test]
    fn dates_are_days_since_1899_12_30() {
        let day = |t: &str| number_of(t).map(|(v, f)| (v, f.code()));
        assert_eq!(day("2024-05-01"), Some((45413.0, "yyyy-mm-dd".to_string())));
        assert_eq!(day("2024/5/1"), Some((45413.0, "yyyy/m/d".to_string())));
        assert_eq!(day("2024.05.01"), Some((45413.0, "yyyy.mm.dd".to_string())));
        assert_eq!(day("2024年5月1日"), Some((45413.0, "yyyy\"年\"m\"月\"d\"日\"".to_string())));
        // Spaces round 年, 月 and 日 (the gazette writes `2024 年 4 月 19 日`) are allowed; elsewhere they are not.
        assert_eq!(day("2024 年 4 月 19 日"), Some((45401.0, "yyyy\"年\"mm\"月\"dd\"日\"".to_string())));
        assert_eq!(day("2024年 4月 19日 10:30").map(|d| d.0), Some(45401.0 + 10.5 / 24.0));
        assert!(day("2024 -05-01").is_none() && day("2024 年 4 月 19").is_none() && day("2024 年 4 月 日").is_none());
        assert_eq!(day("1900-03-01").map(|d| d.0), Some(61.0));
        assert_eq!(day("2000-01-01").map(|d| d.0), Some(36526.0));
        assert_eq!(day("2024-02-29").map(|d| d.0), Some(45351.0));
        assert_eq!(day("2024-05-01 12:00"), Some((45413.5, "yyyy-mm-dd hh:mm".to_string())));
        assert_eq!(day("2024-05-01T06:00:00"), Some((45413.25, "yyyy-mm-dd hh:mm:ss".to_string())));
        assert!(day("2024-05-01 25:00").is_none() && day("2023-02-29").is_none() && day("2024-05-01x").is_none());
    }
}
