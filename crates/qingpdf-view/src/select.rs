//! Selected text (3d-2): from one character to another in reading order, over one page or several. Plain logic; the
//! boxes come from the engine and the drawing is `app.rs`'s.

use qingpdf_core::view::MAX_COPY_PAGES;

/// A character of a page: the page (from 0) and the character's place in the page's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CharPos {
    pub page: u32,
    pub index: u32,
}

/// Most pages a selection spans (what the engine copies in one go).
pub const MAX_SELECTION_PAGES: u32 = MAX_COPY_PAGES as u32;

/// From the character the pointer went down on to the one it is on now, both included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CharPos,
    pub head: CharPos,
}

impl Selection {
    /// The two ends, first (in reading order) then last.
    pub fn ordered(&self) -> (CharPos, CharPos) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    /// Does a selection from `anchor` to `head` stay within the most pages?
    pub fn within_limit(anchor: CharPos, head: CharPos) -> bool {
        anchor.page.abs_diff(head.page) < MAX_SELECTION_PAGES
    }

    /// The characters of `page` that are selected, as `start..end` indexes into the page's `len` characters; `None` when
    /// none of them are.
    pub fn range_in(&self, page: u32, len: usize) -> Option<(usize, usize)> {
        let (first, last) = self.ordered();
        if page < first.page || page > last.page || len == 0 {
            return None;
        }
        let start = if page == first.page { first.index as usize } else { 0 };
        let end = if page == last.page { (last.index as usize).saturating_add(1) } else { len }.min(len);
        (start < end).then_some((start, end))
    }

    /// The places to give [`qingpdf_core::view::Engine::copy_text`]: from the first character, up to but not including the
    /// one after the last.
    pub fn copy_span(&self) -> ((u32, u32), (u32, u32)) {
        let (first, last) = self.ordered();
        ((first.page, first.index), (last.page, last.index.saturating_add(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(page: u32, index: u32) -> CharPos {
        CharPos { page, index }
    }

    #[test]
    fn the_ends_are_put_in_reading_order_and_both_are_included() {
        let a = Selection { anchor: at(2, 10), head: at(1, 40) };
        assert_eq!(a.ordered(), (at(1, 40), at(2, 10)));
        assert_eq!(a.copy_span(), ((1, 40), (2, 11)));
        let one = Selection { anchor: at(0, 5), head: at(0, 5) };
        assert_eq!(one.copy_span(), ((0, 5), (0, 6)));
    }

    #[test]
    fn each_page_has_its_part() {
        let s = Selection { anchor: at(1, 10), head: at(3, 4) };
        assert_eq!(s.range_in(0, 100), None);
        assert_eq!(s.range_in(1, 100), Some((10, 100)));
        assert_eq!(s.range_in(2, 100), Some((0, 100)));
        assert_eq!(s.range_in(3, 100), Some((0, 5)));
        assert_eq!(s.range_in(4, 100), None);
        // An end past the page's characters, a page with none, a selection inside one page.
        assert_eq!(Selection { anchor: at(0, 90), head: at(0, 500) }.range_in(0, 100), Some((90, 100)));
        assert_eq!(s.range_in(2, 0), None);
        assert_eq!(Selection { anchor: at(0, 7), head: at(0, 3) }.range_in(0, 100), Some((3, 8)));
        assert_eq!(Selection { anchor: at(0, 200), head: at(0, 300) }.range_in(0, 100), None);
    }

    #[test]
    fn a_selection_is_held_to_the_most_pages() {
        assert!(Selection::within_limit(at(0, 0), at(MAX_SELECTION_PAGES - 1, 0)));
        assert!(!Selection::within_limit(at(0, 0), at(MAX_SELECTION_PAGES, 0)));
        assert!(Selection::within_limit(at(500, 0), at(450, 0)));
        assert!(!Selection::within_limit(at(500, 0), at(300, 0)));
    }
}
