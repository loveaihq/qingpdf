//! Double click and triple click: which characters make "the word" and "the line" under the pointer. Plain rules over the
//! characters of a page (`CharBoxes::text`, one `char` for each box); the window and the engine are not needed.
//!
//! The rules (also in `docs/decisions.md`):
//! - letters and digits that are not CJK make a word, and so does `_`; an apostrophe between two letters (`don't`) and a `.` or `,`
//!   between two digits (`3.14`, `1,234`) stay inside it;
//! - CJK (Han, kana, Hangul) has no spaces and nothing here knows where its words end, so a double click takes the whole run of
//!   CJK characters round the pointer, up to the punctuation, space, line end or other kind of letter on either side (a clause
//!   in a Chinese sentence, a whole heading that has no punctuation);
//! - on a space the run of spaces is taken, on punctuation the run of the same mark (`……`, `--`);
//! - a triple click takes the line: what lies between two line ends (`'\n'`), without the spaces at its ends.

/// What a character is, for finding words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// The end of a line.
    Break,
    Space,
    Word,
    Cjk,
    Punct,
}

/// Is `c` a Han, kana or Hangul letter (the marks of CJK punctuation and the full-width forms of Latin letters are not)?
fn is_cjk(c: char) -> bool {
    let u = u32::from(c);
    c != '\u{30FB}'
        && matches!(u,
            0x2E80..=0x2FDF // radicals
            | 0x3005..=0x3007 // 々 〆 〇
            | 0x3021..=0x3029 | 0x3038..=0x303B // Hangzhou numerals and marks that are letters
            | 0x3041..=0x30FF // kana
            | 0x3100..=0x312F | 0x31A0..=0x31BF // bopomofo
            | 0x3400..=0x4DBF | 0x4E00..=0x9FFF // Han
            | 0xF900..=0xFAFF // compatibility Han
            | 0xFF66..=0xFF9F // half-width katakana
            | 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF // Hangul
            | 0x20000..=0x2FA1F | 0x30000..=0x3134F // Han, extensions
        )
}

fn class(c: char) -> Class {
    if c == '\n' || c == '\r' {
        Class::Break
    } else if c.is_whitespace() || c == '\u{3000}' {
        Class::Space
    } else if is_cjk(c) {
        Class::Cjk
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// The word at character `at` of `text`, as `start..end` (the end not included); `None` for the end of a line or a place that is not
/// in the text.
pub fn word_range(text: &[char], at: usize) -> Option<(usize, usize)> {
    let c = *text.get(at)?;
    let k = class(c);
    let class_at = |i: usize| text.get(i).map(|&c| class(c));
    match k {
        Class::Break => None,
        Class::Punct => {
            let (mut a, mut b) = (at, at + 1);
            while a > 0 && text.get(a - 1) == Some(&c) {
                a -= 1;
            }
            while text.get(b) == Some(&c) {
                b += 1;
            }
            Some((a, b))
        }
        Class::Space | Class::Cjk => {
            let (mut a, mut b) = (at, at + 1);
            while a > 0 && class_at(a - 1) == Some(k) {
                a -= 1;
            }
            while class_at(b) == Some(k) {
                b += 1;
            }
            Some((a, b))
        }
        Class::Word => {
            let is_word = |i: usize| class_at(i) == Some(Class::Word);
            let is_digit = |i: usize| text.get(i).is_some_and(char::is_ascii_digit);
            // A mark that joins two letters (an apostrophe) or two digits (a point, a comma) stays in the word.
            let joins = |i: usize| match text.get(i) {
                Some('\'' | '\u{2019}') => i > 0 && is_word(i - 1) && is_word(i + 1) && !is_digit(i - 1),
                Some('.' | ',') => i > 0 && is_digit(i - 1) && is_digit(i + 1),
                _ => false,
            };
            let (mut a, mut b) = (at, at + 1);
            while a > 0 && (is_word(a - 1) || joins(a - 1)) {
                a -= 1;
            }
            while is_word(b) || joins(b) {
                b += 1;
            }
            Some((a, b))
        }
    }
}

/// The line at character `at` of `text`, as `start..end` (the end not included), without the spaces at its ends; `None` for the end
/// of a line, a place that is not in the text, or a line of nothing but spaces.
pub fn line_range(text: &[char], at: usize) -> Option<(usize, usize)> {
    if text.get(at).is_none_or(|&c| class(c) == Class::Break) {
        return None;
    }
    let mut a = at;
    while a > 0 && text.get(a - 1).is_some_and(|&c| class(c) != Class::Break) {
        a -= 1;
    }
    let mut b = at + 1;
    while text.get(b).is_some_and(|&c| class(c) != Class::Break) {
        b += 1;
    }
    while a < b && text.get(a).is_some_and(|&c| class(c) == Class::Space) {
        a += 1;
    }
    while b > a && text.get(b - 1).is_some_and(|&c| class(c) == Class::Space) {
        b -= 1;
    }
    (a < b).then_some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn word(s: &str, at: usize) -> Option<String> {
        let t = chars(s);
        word_range(&t, at).map(|(a, b)| t[a..b].iter().collect())
    }

    #[test]
    fn a_latin_word_is_the_letters_and_digits_round_the_pointer() {
        assert_eq!(word("Hello, world 42x!", 1).as_deref(), Some("Hello"));
        assert_eq!(word("Hello, world 42x!", 8).as_deref(), Some("world"));
        assert_eq!(word("Hello, world 42x!", 13).as_deref(), Some("42x"));
        assert_eq!(word("snake_case name", 3).as_deref(), Some("snake_case"));
        assert_eq!(word("x", 0).as_deref(), Some("x"));
    }

    #[test]
    fn an_apostrophe_between_letters_and_a_point_between_digits_stay_in_the_word() {
        assert_eq!(word("it doesn't work", 6).as_deref(), Some("doesn't"));
        assert_eq!(word("it doesn\u{2019}t work", 6).as_deref(), Some("doesn\u{2019}t"));
        assert_eq!(word("pi is 3.14, and 1,234.5 more", 6).as_deref(), Some("3.14"));
        assert_eq!(word("pi is 3.14, and 1,234.5 more", 16).as_deref(), Some("1,234.5"));
        assert_eq!(word("pi is 3.14, and 1,234.5 more", 9).as_deref(), Some("3.14"));
        // At the end of a sentence the point is not part of the word; a quote at the start is not either.
        assert_eq!(word("see page 3. Then", 9).as_deref(), Some("3"));
        assert_eq!(word("'quoted' text", 3).as_deref(), Some("quoted"));
    }

    #[test]
    fn cjk_takes_the_whole_run_up_to_the_punctuation() {
        let s = "关于做好公文处理工作的通知，请各单位遵照执行。";
        assert_eq!(word(s, 4).as_deref(), Some("关于做好公文处理工作的通知"));
        assert_eq!(word(s, 0).as_deref(), Some("关于做好公文处理工作的通知"));
        assert_eq!(word(s, 12).as_deref(), Some("关于做好公文处理工作的通知"));
        // The comma is a mark of its own, then the next clause.
        assert_eq!(word(s, 13).as_deref(), Some("，"));
        assert_eq!(word(s, 15).as_deref(), Some("请各单位遵照执行"));
        assert_eq!(word(s, s.chars().count() - 1).as_deref(), Some("。"));
        assert_eq!(word("これは日本語です", 4).as_deref(), Some("これは日本語です"));
        assert_eq!(word("한국어 문장", 1).as_deref(), Some("한국어"));
    }

    #[test]
    fn latin_next_to_cjk_is_a_word_of_its_own() {
        assert_eq!(word("PDF文件2024年", 1).as_deref(), Some("PDF"));
        assert_eq!(word("PDF文件2024年", 4).as_deref(), Some("文件"));
        assert_eq!(word("PDF文件2024年", 6).as_deref(), Some("2024"));
        assert_eq!(word("PDF文件2024年", 9).as_deref(), Some("年"));
        // Full-width digits and letters are words, not CJK.
        assert_eq!(word("第１２３页", 2).as_deref(), Some("１２３"));
    }

    #[test]
    fn spaces_and_marks_are_taken_as_runs_and_a_line_end_is_nothing() {
        assert_eq!(word("a   b", 2).as_deref(), Some("   "));
        assert_eq!(word("wait... what", 5).as_deref(), Some("..."));
        assert_eq!(word("wait... what", 4).as_deref(), Some("..."));
        assert_eq!(word("a\nb", 1), None);
        assert_eq!(word("abc", 3), None);
        assert_eq!(word("", 0), None);
    }

    #[test]
    fn a_line_is_what_lies_between_line_ends() {
        let t = chars("first line\n  second one  \n\n第三行，有字\nlast");
        let line = |at: usize| line_range(&t, at).map(|(a, b)| t[a..b].iter().collect::<String>());
        assert_eq!(line(0).as_deref(), Some("first line"));
        assert_eq!(line(9).as_deref(), Some("first line"));
        assert_eq!(line(10), None);
        assert_eq!(line(14).as_deref(), Some("second one"));
        assert_eq!(line(11).as_deref(), Some("second one"));
        assert_eq!(line(25), None);
        assert_eq!(line(26), None);
        assert_eq!(line(30).as_deref(), Some("第三行，有字"));
        assert_eq!(line(t.len() - 1).as_deref(), Some("last"));
        assert_eq!(line(t.len()), None);
        assert_eq!(line_range(&chars("   \nx"), 1), None);
    }
}
