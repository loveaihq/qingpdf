//! What a search compares (3d-2). The text of a page and the words typed are both folded the same way, so that
//! - capital and small letters are the same,
//! - full-width and half-width forms are the same (`２０２４` and `2024`, `，` and `,`, the ideographic space and a
//!   space), and the Kangxi radicals and compatibility ideographs count as the ideograph they stand for,
//! - a line end is a space, and a space next to a Chinese or Japanese character is nothing (Chinese lines are joined with
//!   no space, and layout puts a space between a digit and a character when the gap is wide),
//! - runs of spaces are one, and spaces at either end are nothing.
//!
//! Folded text keeps, for each character, where it came from in the page's text, so that a match can be shown on the page.

use crate::text::data::normalize_cjk;

/// The most characters of the words typed that are searched for.
pub const MAX_QUERY_CHARS: usize = 200;

/// A character that is written without spaces round it (Chinese, Japanese, Korean).
fn is_wide(c: char) -> bool {
    matches!(u32::from(c), 0x2E80..=0xA4CF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0x20000..=0x2FFFF)
}

/// One character folded: `None` for one that is left out (soft hyphen).
fn fold_char(c: char) -> Option<char> {
    let c = match c {
        '\u{AD}' => return None,
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        other => other,
    };
    let c = char::from_u32(normalize_cjk(u32::from(c))).unwrap_or(c);
    if c.is_whitespace() {
        return Some(' ');
    }
    Some(if c.is_ascii() { c.to_ascii_lowercase() } else { c.to_lowercase().next().unwrap_or(c) })
}

/// The text folded, and for each folded character the index (in characters) of the one it came from. A space that
/// stands for several (or for a line end) comes from the first.
pub fn fold(text: &str) -> (Vec<char>, Vec<u32>) {
    let mut chars: Vec<char> = Vec::with_capacity(text.len() / 2 + 1);
    let mut origin: Vec<u32> = Vec::with_capacity(text.len() / 2 + 1);
    // A space seen and not yet written: it is written only when it ends up between two narrow characters.
    let mut pending: Option<u32> = None;
    for (i, c) in text.chars().enumerate() {
        let Some(f) = fold_char(c) else { continue };
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if f == ' ' {
            if !chars.is_empty() && pending.is_none() {
                pending = Some(i);
            }
            continue;
        }
        if let Some(at) = pending.take()
            && !chars.last().is_some_and(|&p| is_wide(p))
            && !is_wide(f)
        {
            chars.push(' ');
            origin.push(at);
        }
        chars.push(f);
        origin.push(i);
    }
    (chars, origin)
}

/// The words typed, folded and cut to [`MAX_QUERY_CHARS`]. Empty: nothing to look for.
pub fn fold_query(query: &str) -> Vec<char> {
    let (mut chars, _) = fold(query);
    chars.truncate(MAX_QUERY_CHARS);
    if chars.last() == Some(&' ') {
        chars.pop();
    }
    chars
}

/// Where `needle` is in `hay`, as (start, end) pairs of indexes, in order, not overlapping, at most `max` of them.
/// `steps` is told, now and then, how many characters were looked at since it was last told (the caller charges them
/// to a work meter); when it answers `false` the search stops and what was found so far is returned.
pub fn find_all(hay: &[char], needle: &[char], max: usize, steps: &mut dyn FnMut(usize) -> bool) -> Vec<(usize, usize)> {
    const BATCH: usize = 8192;
    let mut found = Vec::new();
    let Some((&first, rest)) = needle.split_first() else { return found };
    let m = needle.len();
    let (mut i, mut looked) = (0usize, 0usize);
    while i + m <= hay.len() && found.len() < max {
        // The next place the first character is, found by a plain scan.
        let window = hay.get(i..=hay.len() - m).unwrap_or(&[]);
        let skip = window.iter().position(|&c| c == first);
        let Some(skip) = skip else {
            looked += window.len();
            break;
        };
        looked += skip + 1;
        i += skip;
        if hay.get(i + 1..i + m).is_some_and(|tail| tail == rest) {
            found.push((i, i + m));
            looked += m;
            i += m;
        } else {
            // Count what was compared, roughly: the characters that agreed with the start of the needle.
            looked += hay.get(i + 1..i + m).map_or(0, |tail| tail.iter().zip(rest).take_while(|(a, b)| a == b).count());
            i += 1;
        }
        if looked >= BATCH {
            if !steps(looked) {
                return found;
            }
            looked = 0;
        }
    }
    let _ = steps(looked);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, query: &str) -> Vec<(usize, usize)> {
        let (hay, origin) = fold(text);
        let needle = fold_query(query);
        find_all(&hay, &needle, 1000, &mut |_| true)
            .into_iter()
            .map(|(s, e)| (origin[s] as usize, origin[e - 1] as usize + 1))
            .collect()
    }

    #[test]
    fn latin_is_found_whatever_the_case() {
        assert_eq!(find("Hello World, hello world", "HELLO"), vec![(0, 5), (13, 18)]);
        assert_eq!(find("Straße", "STRAßE"), vec![(0, 6)]);
        assert_eq!(find("abc", ""), vec![]);
        assert_eq!(find("abc", "abcd"), vec![]);
        assert_eq!(find("aaaa", "aa"), vec![(0, 2), (2, 4)]);
    }

    #[test]
    fn full_width_and_half_width_are_the_same() {
        // A query typed half-width finds the full-width text, and the other way round.
        assert_eq!(find("２０２４年度，计划", "2024年度,计划"), vec![(0, 9)]);
        assert_eq!(find("2024年度,计划", "２０２４年度，计划"), vec![(0, 9)]);
        assert_eq!(find("ＡＢＣ　ｄｅｆ", "abc DEF"), vec![(0, 7)]);
        assert_eq!(find("（试行）", "(试行)"), vec![(0, 4)]);
    }

    #[test]
    fn compatibility_ideographs_count_as_the_ideograph() {
        // U+2F08 (Kangxi radical person) in the text, U+4EBA typed.
        assert_eq!(find("\u{2F08}民", "人民"), vec![(0, 2)]);
        assert_eq!(find("人民", "\u{F900}"), vec![]);
        assert_eq!(find("豈", "\u{F900}"), vec![(0, 1)]);
    }

    #[test]
    fn lines_are_joined_without_a_space_for_chinese_and_with_one_for_latin() {
        assert_eq!(find("国务院办\n公厅关于", "办公厅"), vec![(3, 7)]);
        assert_eq!(find("the quick\nbrown fox", "quick brown"), vec![(4, 15)]);
        assert_eq!(find("the quick   \n   brown", "quick brown"), vec![(4, 21)]);
        // A space between a digit and a Chinese character is not in the way, nor is one the query has.
        assert_eq!(find("共 3 件", "共3件"), vec![(0, 5)]);
        assert_eq!(find("共3件", "共 3 件"), vec![(0, 3)]);
        // Spaces at either end of the query are nothing.
        assert_eq!(find("quick brown", "  quick brown "), vec![(0, 11)]);
    }

    #[test]
    fn typographic_quotes_match_plain_ones() {
        assert_eq!(find("don\u{2019}t say \u{201C}no\u{201D}", "don't say \"no\""), vec![(0, 14)]);
    }

    #[test]
    fn the_query_is_cut_and_the_work_is_told() {
        let long: String = "a".repeat(500);
        assert_eq!(fold_query(&long).len(), MAX_QUERY_CHARS);
        let hay: Vec<char> = "ab".repeat(50_000).chars().collect();
        let needle: Vec<char> = "ba".chars().collect();
        let mut told = 0usize;
        let found = find_all(&hay, &needle, usize::MAX, &mut |n| {
            told += n;
            true
        });
        assert_eq!(found.len(), 49_999);
        assert!(told >= hay.len() / 2, "the work told ({told}) is in proportion to the text");
        // A meter that says stop ends the search with what was found.
        let mut calls = 0;
        let few = find_all(&hay, &needle, usize::MAX, &mut |_| {
            calls += 1;
            calls < 3
        });
        assert!(few.len() < found.len() && !few.is_empty());
        // The most hits.
        assert_eq!(find_all(&hay, &needle, 10, &mut |_| true).len(), 10);
    }

    #[test]
    fn a_hostile_text_costs_in_proportion_to_what_was_compared() {
        // Many near matches: every start agrees with 199 characters of the needle.
        let hay: Vec<char> = "a".repeat(100_000).chars().collect();
        let needle: Vec<char> = format!("{}b", "a".repeat(199)).chars().collect();
        let mut told = 0usize;
        let found = find_all(&hay, &needle, 10, &mut |n| {
            told += n;
            true
        });
        assert!(found.is_empty());
        assert!(told > 10_000_000, "the near matches are charged: {told}");
    }
}
