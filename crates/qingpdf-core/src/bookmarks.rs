//! Editing the bookmarks as a list (2026-10-20): the plain logic, with no file and no window in it.
//!
//! The bookmarks are a flat list in display order, each item with its depth (`view::OutlineItem`); an item's children are the items
//! after it that are deeper, up to the next that is not. Adding, taking out, moving (before, after or inside another item, never into
//! its own branch), making an item one level deeper or shallower, the `/Count` of every item (12.3.3), the text of the command line's
//! `bookmarks --export` and `--import`, and the bookmarks made from the headings of a document are all done here.

use crate::view::{MAX_OUTLINE_DEPTH, MAX_OUTLINE_ITEMS, MAX_TITLE_CHARS, OutlineItem};

/// The biggest text file `bookmarks --import` reads, in bytes.
pub const MAX_IMPORT_BYTES: u64 = 4 << 20;
/// The deepest level of bookmarks made from headings.
pub const MAX_HEADING_LEVELS: u8 = 3;
/// What `export_text` writes for a bookmark with no title (or one of blanks only), which an import would not take: the word the reader's
/// tree shows (in English: the command line has one language).
pub const UNTITLED: &str = "(untitled)";

/// Where a bookmark that is moved goes, in relation to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Before,
    After,
    /// As the last of its children.
    Inside,
}

/// One past the last item of the branch that starts at `i` (the item and all that is below it).
pub fn branch_end(items: &[OutlineItem], i: usize) -> usize {
    let Some(first) = items.get(i) else { return i };
    let below = items.get(i + 1..).unwrap_or(&[]).iter().take_while(|x| x.depth > first.depth).count();
    i + 1 + below
}

/// The item that `i` is a child of.
pub fn parent_of(items: &[OutlineItem], i: usize) -> Option<usize> {
    let depth = items.get(i)?.depth;
    items.get(..i)?.iter().rposition(|x| x.depth < depth)
}

/// The depth of the deepest item of the branch that starts at `i`.
fn deepest(items: &[OutlineItem], i: usize) -> u32 {
    items.get(i..branch_end(items, i)).unwrap_or(&[]).iter().map(|x| x.depth).max().unwrap_or(0)
}

/// Is the list a list of bookmarks: not too many, not too deep, the first at the top and no item more than one level deeper than the
/// one before it?
pub fn check(items: &[OutlineItem]) -> Result<(), String> {
    if items.len() > MAX_OUTLINE_ITEMS {
        return Err(format!("more than {MAX_OUTLINE_ITEMS} bookmarks"));
    }
    let mut before: Option<u32> = None;
    for (i, item) in items.iter().enumerate() {
        let ok = match before {
            None => item.depth == 0,
            Some(b) => item.depth <= b + 1,
        };
        if !ok {
            return Err(format!("bookmark {} is at level {} where the one before is at {}", i + 1, item.depth + 1, before.map_or(0, |b| b + 1)));
        }
        if item.depth as usize >= MAX_OUTLINE_DEPTH {
            return Err(format!("bookmarks are at most {MAX_OUTLINE_DEPTH} levels deep"));
        }
        if item.title.chars().count() > MAX_TITLE_CHARS {
            return Err(format!("a title is at most {MAX_TITLE_CHARS} characters"));
        }
        before = Some(item.depth);
    }
    Ok(())
}

/// Put `item` after the item `after` and all that is below it, at its level; with no item chosen, at the end of the top level. Gives
/// its place, or `None` if there are as many bookmarks as there may be.
pub fn add(items: &mut Vec<OutlineItem>, after: Option<usize>, mut item: OutlineItem) -> Option<usize> {
    if items.len() >= MAX_OUTLINE_ITEMS {
        return None;
    }
    let at = match after.filter(|&i| i < items.len()) {
        Some(i) => {
            item.depth = items.get(i).map_or(0, |x| x.depth);
            branch_end(items, i)
        }
        None => {
            item.depth = 0;
            items.len()
        }
    };
    items.insert(at, item);
    Some(at)
}

/// Take out the item and all that is below it. Gives how many went.
pub fn remove(items: &mut Vec<OutlineItem>, i: usize) -> usize {
    if i >= items.len() {
        return 0;
    }
    let end = branch_end(items, i);
    items.drain(i..end).count()
}

/// Can the item be made a child of the item before it (which must be at its level or below it, and the branch must stay within the
/// depth that is allowed)?
pub fn can_indent(items: &[OutlineItem], i: usize) -> bool {
    i > 0 && items.get(i - 1).zip(items.get(i)).is_some_and(|(before, item)| before.depth >= item.depth) && (deepest(items, i) as usize) + 1 < MAX_OUTLINE_DEPTH
}

/// Make the item (and what is below it) one level deeper: a child of the sibling before it, which is then shown open.
pub fn indent(items: &mut [OutlineItem], i: usize) -> bool {
    if !can_indent(items, i) {
        return false;
    }
    let depth = items.get(i).map_or(0, |x| x.depth);
    let end = branch_end(items, i);
    for x in items.get_mut(i..end).unwrap_or(&mut []) {
        x.depth += 1;
    }
    // The sibling before it: the nearest item before that is as deep as it was.
    if let Some(sibling) = items.get(..i).and_then(|before| before.iter().rposition(|x| x.depth == depth))
        && let Some(s) = items.get_mut(sibling)
    {
        s.open = true;
    }
    true
}

pub fn can_outdent(items: &[OutlineItem], i: usize) -> bool {
    items.get(i).is_some_and(|x| x.depth > 0)
}

/// Make the item (and what is below it) one level shallower, where it is in the list: the items after it that were its brothers are now
/// below it.
pub fn outdent(items: &mut [OutlineItem], i: usize) -> bool {
    if !can_outdent(items, i) {
        return false;
    }
    let end = branch_end(items, i);
    for x in items.get_mut(i..end).unwrap_or(&mut []) {
        x.depth -= 1;
    }
    true
}

/// Move the item `from` and all that is below it to `place` of the item `to`. Gives where the item is now, or `None` when it is not
/// done: `to` is in the branch that moves (that would be a cycle), the move changes nothing, or the branch would be too deep.
pub fn move_branch(items: &mut Vec<OutlineItem>, from: usize, to: usize, place: Place) -> Option<usize> {
    let end = branch_end(items, from);
    if from >= items.len() || to >= items.len() || (from..end).contains(&to) {
        return None;
    }
    let (to_depth, to_end) = (items.get(to)?.depth, branch_end(items, to));
    let (at, depth) = match place {
        Place::Before => (to, to_depth),
        Place::After => (to_end, to_depth),
        Place::Inside => (to_end, to_depth + 1),
    };
    // Where it is already: in front of the item it goes before, or at the end of the branch it goes after.
    let same_place = at == from || at == end;
    let root = items.get(from)?.depth;
    if same_place && depth == root {
        return None;
    }
    if deepest(items, from) - root + depth >= MAX_OUTLINE_DEPTH as u32 {
        return None;
    }
    let mut branch: Vec<OutlineItem> = items.drain(from..end).collect();
    for x in &mut branch {
        x.depth = x.depth - root + depth;
    }
    // (The place is by the list as it was; the branch that was taken out may have been in front of it.)
    let at = if at >= end { at - branch.len() } else { at };
    drop(items.splice(at..at, branch));
    if place == Place::Inside
        && let Some(parent) = parent_of(items, at)
        && let Some(p) = items.get_mut(parent)
    {
        p.open = true;
    }
    Some(at)
}

/// The `/Count` of every item (12.3.3: the number of items below it that are shown, when it is open; the same number made negative when it
/// is closed; none for an item without children, given as 0), and the `/Count` of the outline itself (the number of items that are shown).
pub fn counts(items: &[OutlineItem]) -> (Vec<i64>, i64) {
    let n = items.len();
    // The parent of every item, from one pass over the list.
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut chain: Vec<usize> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        chain.truncate(item.depth as usize);
        parents.push(chain.last().copied());
        chain.push(i);
    }
    // What is shown below each item when it is open, from the last item to the first (the items below one come after it).
    let mut shown = vec![0i64; n];
    let mut root = 0i64;
    for i in (0..n).rev() {
        let below = shown.get(i).copied().unwrap_or(0);
        let open = items.get(i).is_some_and(|x| x.open);
        let add = 1 + if open { below } else { 0 };
        match parents.get(i).copied().flatten().and_then(|p| shown.get_mut(p)) {
            Some(p) => *p += add,
            None => root += add,
        }
    }
    let counts = items
        .iter()
        .zip(&shown)
        .map(|(item, &below)| match (below, item.open) {
            (0, _) => 0,
            (n, true) => n,
            (n, false) => -n,
        })
        .collect();
    (counts, root)
}

// --- the text of `bookmarks --export` and `--import` ------------------------------------------------------------------------------

/// A title for a line of text: control characters (tabs and line ends too) are spaces.
fn one_line(title: &str) -> String {
    title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// One line for each bookmark: a tab for each level above the top, the title, a tab, and the page (from 1; none for a bookmark that leads
/// nowhere that could be found).
pub fn export_text(items: &[OutlineItem]) -> String {
    let mut out = String::new();
    for item in items {
        out.extend(std::iter::repeat_n('\t', item.depth as usize));
        let title = one_line(&item.title);
        out.push_str(if title.trim().is_empty() { UNTITLED } else { &title });
        out.push('\t');
        if let Some(page) = item.page {
            out.push_str(&(u64::from(page) + 1).to_string());
        }
        out.push('\n');
    }
    out
}

/// The bookmarks in `text` (as `export_text` writes them; a byte order mark at the start and blank lines are not minded) for a document of
/// `pages` pages. A level is a tab: a line that starts with a space (or another blank) is refused, for taking spaces as part of the title would
/// make a title of what is meant as a level; the title is the words up to the last tab, with the blanks at its ends taken off, and a tab inside
/// it is refused (a line with a tab after the page number would otherwise read the number as part of the title). A line that is not right
/// is an error that says which.
pub fn parse_text(text: &str, pages: usize) -> Result<Vec<OutlineItem>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut items: Vec<OutlineItem> = Vec::new();
    for (k, line) in text.split('\n').enumerate() {
        let (number, line) = (k + 1, line.strip_suffix('\r').unwrap_or(line));
        if line.trim().is_empty() {
            continue;
        }
        let depth = line.chars().take_while(|&c| c == '\t').count();
        let rest = line.get(depth..).unwrap_or("");
        let Some((title, page)) = rest.rsplit_once('\t') else {
            return Err(format!("line {number}: expected the title, a tab and the page number"));
        };
        if title.trim().is_empty() {
            return Err(format!("line {number}: the title is empty"));
        }
        if rest.starts_with(char::is_whitespace) {
            return Err(format!("line {number}: the line starts with a space; levels are written with tabs (one tab for each level), and a title does not start with a space"));
        }
        let title = title.trim();
        if title.contains('\t') {
            return Err(format!("line {number}: there is a tab inside the title (a tab ends the title: the page number comes after it, and nothing after that)"));
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(format!("line {number}: the title is more than {MAX_TITLE_CHARS} characters"));
        }
        if depth >= MAX_OUTLINE_DEPTH {
            return Err(format!("line {number}: more than {MAX_OUTLINE_DEPTH} levels"));
        }
        let before = items.last().map(|x| x.depth as usize);
        if depth > before.map_or(0, |b| b + 1) {
            return Err(format!("line {number}: the level jumps (there are {depth} tabs, and the bookmark before has {})", before.unwrap_or(0)));
        }
        if items.len() >= MAX_OUTLINE_ITEMS {
            return Err(format!("line {number}: more than {MAX_OUTLINE_ITEMS} bookmarks"));
        }
        let page = page.trim();
        let page = if page.is_empty() {
            None
        } else {
            match page.parse::<u64>() {
                Ok(p) if p >= 1 && p as usize <= pages => Some((p - 1) as u32),
                Ok(_) => return Err(format!("line {number}: page {page} is not in the file (it has {pages} page{})", if pages == 1 { "" } else { "s" })),
                Err(_) => return Err(format!("line {number}: '{page}' is not a page number")),
            }
        };
        items.push(OutlineItem::made(&one_line(title), depth as u32, page, None, None));
    }
    Ok(items)
}

// --- bookmarks from headings ----------------------------------------------------------------------------------------------------

/// A heading found in a document: the page it is on (from 0), its level (1 is the biggest), its words and how far down the page it is
/// (points from the top of the page as shown).
#[derive(Clone, Debug, PartialEq)]
pub struct Heading {
    pub page: u32,
    pub level: u8,
    pub text: String,
    pub y: Option<f32>,
}

/// How far above the line of a heading the bookmark leads, in points.
const HEADING_ABOVE: f32 = 6.0;

/// The bookmarks for headings, in the order they are given: only the first [`MAX_HEADING_LEVELS`] levels, the words made into one
/// line and cut to the length a title may have, and no level deeper than one below the one before (a document may start with a
/// second-level heading). Gives them and whether some were left out for being too many.
pub fn from_headings(headings: &[Heading]) -> (Vec<OutlineItem>, bool) {
    let mut items: Vec<OutlineItem> = Vec::new();
    let mut left_out = false;
    for h in headings.iter().filter(|h| (1..=MAX_HEADING_LEVELS).contains(&h.level)) {
        let words = h.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let title: String = one_line(&words).chars().take(MAX_TITLE_CHARS).collect();
        if title.is_empty() {
            continue;
        }
        if items.len() >= MAX_OUTLINE_ITEMS {
            left_out = true;
            break;
        }
        let wanted = u32::from(h.level) - 1;
        let depth = wanted.min(items.last().map_or(0, |x| x.depth + 1));
        let y = h.y.map(|y| (y - HEADING_ABOVE).max(0.0));
        items.push(OutlineItem::made(&title, depth, Some(h.page), None, Some(y.unwrap_or(0.0))));
    }
    (items, left_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(spec: &str) -> Vec<OutlineItem> {
        // "a b .c" style: a title with a leading dot for each level.
        spec.split_whitespace()
            .map(|w| {
                let depth = w.chars().take_while(|&c| c == '.').count();
                OutlineItem::made(w.trim_start_matches('.'), depth as u32, Some(0), None, None)
            })
            .collect()
    }

    fn show(items: &[OutlineItem]) -> String {
        items.iter().map(|x| format!("{}{}", ".".repeat(x.depth as usize), x.title)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn a_branch_is_the_item_and_the_deeper_items_after_it() {
        let l = list("a .b ..c .d e");
        assert_eq!(branch_end(&l, 0), 4);
        assert_eq!(branch_end(&l, 1), 3);
        assert_eq!(branch_end(&l, 3), 4);
        assert_eq!(branch_end(&l, 4), 5);
        assert_eq!(branch_end(&l, 9), 9);
        assert_eq!(parent_of(&l, 2), Some(1));
        assert_eq!(parent_of(&l, 3), Some(0));
        assert_eq!(parent_of(&l, 4), None);
    }

    #[test]
    fn a_new_bookmark_goes_after_the_branch_of_the_chosen_one_or_at_the_end() {
        let mut l = list("a .b ..c d");
        let new = |t: &str| OutlineItem::made(t, 7, Some(1), None, None);
        assert_eq!(add(&mut l, Some(1), new("x")), Some(3));
        assert_eq!(show(&l), "a .b ..c .x d");
        assert_eq!(add(&mut l, Some(0), new("y")), Some(4));
        assert_eq!(show(&l), "a .b ..c .x y d");
        assert_eq!(add(&mut l, None, new("z")), Some(l.len() - 1));
        assert_eq!(l.last().map(|x| (x.title.as_str(), x.depth)), Some(("z", 0)));
        let mut empty: Vec<OutlineItem> = Vec::new();
        assert_eq!(add(&mut empty, Some(3), new("only")), Some(0));
        assert_eq!(empty.len(), 1);
    }

    #[test]
    fn removing_takes_the_children_too() {
        let mut l = list("a .b ..c .d e");
        assert_eq!(remove(&mut l, 1), 2);
        assert_eq!(show(&l), "a .d e");
        assert_eq!(remove(&mut l, 0), 2);
        assert_eq!(show(&l), "e");
        assert_eq!(remove(&mut l, 5), 0);
    }

    #[test]
    fn indent_makes_a_child_of_the_brother_before_and_outdent_gives_the_level_back() {
        let mut l = list("a b .c d");
        assert!(indent(&mut l, 1));
        assert_eq!(show(&l), "a .b ..c d");
        assert!(l[0].open, "the new parent shows its child");
        // The first item has no brother before it; one right after its own parent cannot go deeper either.
        assert!(!indent(&mut l, 0));
        let mut m = list("a .b");
        assert!(!indent(&mut m, 1), "no brother before it");
        assert!(outdent(&mut l, 2));
        assert_eq!(show(&l), "a .b .c d");
        assert!(outdent(&mut l, 1));
        assert_eq!(show(&l), "a b .c d");
        assert!(!outdent(&mut l, 0));
        check(&l).unwrap();
    }

    #[test]
    fn outdenting_keeps_the_order_the_later_brothers_come_below_it() {
        let mut l = list("a .b .c .d");
        assert!(outdent(&mut l, 2));
        assert_eq!(show(&l), "a .b c .d");
        check(&l).unwrap();
    }

    #[test]
    fn the_depth_limit_stops_indent() {
        let mut l: Vec<OutlineItem> = (0..MAX_OUTLINE_DEPTH).map(|d| OutlineItem::made("x", d as u32, Some(0), None, None)).collect();
        l.push(OutlineItem::made("y", (MAX_OUTLINE_DEPTH - 1) as u32, Some(0), None, None));
        let n = l.len() - 1;
        assert!(!can_indent(&l, n), "already at the deepest level");
        check(&l).unwrap();
        l[n].depth = MAX_OUTLINE_DEPTH as u32;
        assert!(check(&l).is_err());
    }

    #[test]
    fn a_branch_moves_before_after_or_inside_another_item_and_never_into_itself() {
        let base = list("a .b ..c d .e f");
        let go = |from: usize, to: usize, place: Place| {
            let mut l = base.clone();
            let at = move_branch(&mut l, from, to, place);
            if at.is_some() {
                check(&l).unwrap();
            }
            (at, show(&l))
        };
        assert_eq!(go(5, 0, Place::Before), (Some(0), "f a .b ..c d .e".to_string()));
        assert_eq!(go(0, 5, Place::After), (Some(3), "d .e f a .b ..c".to_string()));
        assert_eq!(go(3, 0, Place::Inside), (Some(3), "a .b ..c .d ..e f".to_string()));
        assert_eq!(go(1, 5, Place::Inside), (Some(4), "a d .e f .b ..c".to_string()));
        assert_eq!(go(1, 4, Place::Before), (Some(2), "a d .b ..c .e f".to_string()));
        // Into its own branch, onto itself: refused.
        assert_eq!(go(0, 2, Place::Inside).0, None);
        assert_eq!(go(0, 0, Place::After).0, None);
        assert_eq!(go(1, 2, Place::Before).0, None);
        // Where it already is.
        assert_eq!(go(3, 5, Place::Before).0, None);
        // Where it is, but one level down.
        assert_eq!(go(3, 2, Place::After), (Some(3), "a .b ..c ..d ...e f".to_string()));
        assert_eq!(go(9, 0, Place::Before).0, None);
    }

    #[test]
    fn a_move_inside_opens_the_item_it_goes_into() {
        let mut l = list("a b");
        assert_eq!(move_branch(&mut l, 1, 0, Place::Inside), Some(1));
        assert!(l[0].open);
        assert_eq!(show(&l), "a .b");
    }

    #[test]
    fn a_move_that_would_go_too_deep_is_refused() {
        let mut l: Vec<OutlineItem> = (0..MAX_OUTLINE_DEPTH - 1).map(|d| OutlineItem::made("x", d as u32, Some(0), None, None)).collect();
        l.push(OutlineItem::made("a", 0, Some(0), None, None));
        l.push(OutlineItem::made("b", 1, Some(0), None, None));
        let (a, last) = (l.len() - 2, l.len() - 3);
        assert_eq!(move_branch(&mut l, a, last, Place::Inside), None);
        let before = l.len();
        assert_eq!(move_branch(&mut l, a, MAX_OUTLINE_DEPTH - 3, Place::After), Some(a));
        assert_eq!(l.len(), before);
        check(&l).unwrap();
    }

    fn with_open(spec: &str, open: &[usize]) -> Vec<OutlineItem> {
        let mut l = list(spec);
        for &i in open {
            l[i].open = true;
        }
        l
    }

    #[test]
    fn the_count_of_an_item_is_what_is_shown_below_it_negative_when_it_is_closed() {
        // a (open) has b (closed, with c, d), and e. The outline shows a, b, e.
        let l = with_open("a .b ..c ..d .e f", &[0]);
        let (c, root) = counts(&l);
        assert_eq!(c, vec![2, -2, 0, 0, 0, 0]);
        assert_eq!(root, 4);
        // With b open too, the two below it are shown as well.
        let l = with_open("a .b ..c ..d .e f", &[0, 1]);
        let (c, root) = counts(&l);
        assert_eq!(c, vec![4, 2, 0, 0, 0, 0]);
        assert_eq!(root, 6);
        // A closed item hides what is below, however open those are.
        let l = with_open("a .b ..c ..d", &[1]);
        let (c, root) = counts(&l);
        assert_eq!(c, vec![-3, 2, 0, 0]);
        assert_eq!(root, 1);
        assert_eq!(counts(&[]), (vec![], 0));
    }

    #[test]
    fn the_text_of_a_list_comes_back_as_the_list() {
        let mut l = list("Chapter .Section ..Part .Other Last");
        l[0].page = Some(0);
        l[1].page = Some(2);
        l[2].page = None;
        l[3].page = Some(11);
        l[4].page = Some(11);
        let text = export_text(&l);
        assert_eq!(text, "Chapter\t1\n\tSection\t3\n\t\tPart\t\n\tOther\t12\nLast\t12\n");
        let back = parse_text(&text, 12).unwrap();
        assert_eq!(back.iter().map(|x| (x.title.as_str(), x.depth, x.page)).collect::<Vec<_>>(), l.iter().map(|x| (x.title.as_str(), x.depth, x.page)).collect::<Vec<_>>());
        assert_eq!(export_text(&back), text);
    }

    #[test]
    fn the_text_may_start_with_a_byte_order_mark_have_dos_line_ends_and_blank_lines() {
        let got = parse_text("\u{feff}One\t1\r\n\r\n\tTwo words\t2\r\n  \n中文标题\t3", 3).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!((got[1].title.as_str(), got[1].depth, got[1].page), ("Two words", 1, Some(1)));
        assert_eq!(got[2].title, "中文标题");
    }

    #[test]
    fn a_bad_line_is_told_by_its_number() {
        let bad = |text: &str, expect: &str| {
            let e = parse_text(text, 5).unwrap_err();
            assert!(e.contains(expect), "{e:?} should say {expect:?}");
        };
        bad("A\t1\nB without a page\n", "line 2: expected");
        bad("A\t1\nB\tx\n", "line 2: 'x' is not a page number");
        bad("A\t1\nB\t6\n", "line 2: page 6 is not in the file (it has 5 pages)");
        bad("A\t0\n", "line 1: page 0 is not in the file");
        bad("\tA\t1\n", "line 1: the level jumps");
        bad("A\t1\n\t\tB\t1\n", "line 2: the level jumps");
        bad("\t\n\n\t\t\tA\t1", "line 3");
        bad(&format!("{}A\t1", "\t".repeat(MAX_OUTLINE_DEPTH + 1)), "levels");
        bad("A\t1\n\t\t\n\tB\t99999999999999999999999\n", "line 3");
        bad("   \t1\n", "line 1: the title is empty");
        bad(&format!("{}\t1", "x".repeat(MAX_TITLE_CHARS + 1)), "line 1: the title is more than");
        assert_eq!(parse_text("A\t1\n", 1).unwrap().len(), 1);
    }

    #[test]
    fn a_title_of_nothing_is_exported_as_a_word_and_the_text_comes_back() {
        let mut l = list("a b c");
        l[0].title = String::new();
        l[1].title = "\u{a0}\u{3000} ".to_string();
        l[2].title = "\u{7}".to_string();
        let text = export_text(&l);
        assert_eq!(text, "(untitled)\t1\n(untitled)\t1\n(untitled)\t1\n");
        let back = parse_text(&text, 1).unwrap();
        assert_eq!(back.len(), 3);
        assert!(back.iter().all(|x| x.title == UNTITLED && x.page == Some(0)));
        assert_eq!(export_text(&back), text);
    }

    #[test]
    fn a_title_is_trimmed_and_spaces_for_levels_and_a_tab_in_the_title_are_refused() {
        let got = parse_text("Title \u{a0}\t3\n\t Sub\t4\u{a0}\n", 9);
        // (A blank at the start of a title after the tabs is a level written with spaces.)
        let e = got.unwrap_err();
        assert!(e.contains("line 2") && e.contains("starts with a space"), "{e}");
        let ok = parse_text("Title \u{a0}\t3\n\tSub  \t 4 \n", 9).unwrap();
        assert_eq!((ok[0].title.as_str(), ok[1].title.as_str(), ok[1].page), ("Title", "Sub", Some(3)));
        let bad = |text: &str, line: &str, what: &str| {
            let e = parse_text(text, 9).unwrap_err();
            assert!(e.contains(line) && e.contains(what), "{text:?}: {e}");
        };
        bad("  Indented\t1", "line 1", "starts with a space");
        bad("A\t1\n    B\t1\n", "line 2", "tabs");
        bad("A\t1\n \tB\t1\n", "line 2", "starts with a space");
        bad("\u{3000}A\t1\n", "line 1", "starts with a space");
        bad("Title\t3\t", "line 1", "tab inside the title");
        bad("A\t1\nTitle\t3\t\n", "line 2", "tab inside the title");
        bad("a\tb\t3", "line 1", "tab inside the title");
        // Nothing but blanks before the tab is still an empty title.
        bad("  \t1", "line 1", "the title is empty");
    }

    #[test]
    fn a_title_with_a_tab_or_a_line_end_is_one_line_in_the_text() {
        let l = vec![OutlineItem::made("a\tb\nc", 0, Some(0), None, None)];
        let text = export_text(&l);
        assert_eq!(text, "a b c\t1\n");
    }

    fn heading(page: u32, level: u8, text: &str) -> Heading {
        Heading { page, level, text: text.to_string(), y: Some(100.0) }
    }

    #[test]
    fn headings_become_bookmarks_of_at_most_three_levels_with_no_level_skipped() {
        let h = [heading(0, 2, "  Foreword  "), heading(0, 1, "Part\n one"), heading(1, 3, "Deep"), heading(2, 4, "too deep"), heading(2, 1, "   "), heading(3, 3, "Skips a level")];
        let (items, left_out) = from_headings(&h);
        assert!(!left_out);
        assert_eq!(items.iter().map(|x| (x.title.as_str(), x.depth, x.page)).collect::<Vec<_>>(), vec![("Foreword", 0, Some(0)), ("Part one", 0, Some(0)), ("Deep", 1, Some(1)), ("Skips a level", 2, Some(3))]);
        assert!(items.iter().all(|x| x.placed && x.id == 0 && x.y == Some(94.0)));
        check(&items).unwrap();
    }

    #[test]
    fn too_many_headings_are_cut_and_said() {
        let h: Vec<Heading> = (0..MAX_OUTLINE_ITEMS + 5).map(|i| heading(i as u32, 1, "h")).collect();
        let (items, left_out) = from_headings(&h);
        assert_eq!(items.len(), MAX_OUTLINE_ITEMS);
        assert!(left_out);
    }
}
