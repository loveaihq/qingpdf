//! Where a bookmark dragged in the tree lands (2026-10-20), as plain arithmetic: which part of a row the pointer is on, what that
//! means for the place the item gets, and when the tree scrolls. The window gives the numbers (the pointer's place in the tree and the
//! rectangle of the row under it) and does the drawing; nothing here calls Windows, so it can be tested without a window.

use qingpdf_core::bookmarks::Place;

/// A part of a row of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Top,
    Middle,
    Bottom,
}

/// The part of a row (from `top` to `bottom`, in pixels) that the pointer at `y` is on: the top third, the middle third, the bottom third. A
/// place above the row is its top, and one below it its bottom.
pub fn zone(y: i32, top: i32, bottom: i32) -> Zone {
    match y.saturating_sub(top).saturating_mul(3) / bottom.saturating_sub(top).max(1) {
        i32::MIN..=0 => Zone::Top,
        1 => Zone::Middle,
        _ => Zone::Bottom,
    }
}

/// What a drop on `zone` of a row does: the top third puts the item before the row, the middle inside it, and the bottom third after it —
/// before the first child when the row is open and has children (the line is drawn between them, and the item becomes the first of its children).
/// The answer is the place, and whether it is in relation to the first child and not to the row.
pub fn landing(zone: Zone, open_with_children: bool) -> (Place, bool) {
    match zone {
        Zone::Top => (Place::Before, false),
        Zone::Middle => (Place::Inside, false),
        Zone::Bottom if open_with_children => (Place::Before, true),
        Zone::Bottom => (Place::After, false),
    }
}

/// Which way the tree scrolls while an item is held at `y` of a client area `height` pixels high: `Some(false)` up (the pointer is within
/// `edge` pixels of the top, or above the tree), `Some(true)` down, `None` not at all.
pub fn scroll_toward(y: i32, height: i32, edge: i32) -> Option<bool> {
    if y < edge {
        Some(false)
    } else if y > height.saturating_sub(edge) {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_has_a_top_a_middle_and_a_bottom_third() {
        // A row 21 pixels high, from 100 to 121.
        let at = |y: i32| zone(y, 100, 121);
        assert_eq!((at(100), at(106), at(107)), (Zone::Top, Zone::Top, Zone::Middle));
        assert_eq!((at(113), at(114), at(120)), (Zone::Middle, Zone::Bottom, Zone::Bottom));
        // Above the row is its top, below it its bottom; a row that has no height does not divide by zero.
        assert_eq!((at(0), at(500), at(i32::MIN), at(i32::MAX)), (Zone::Top, Zone::Bottom, Zone::Top, Zone::Bottom));
        assert_eq!(zone(5, 5, 5), Zone::Top);
    }

    #[test]
    fn a_drop_goes_before_inside_or_after_and_after_an_open_branch_it_is_the_first_child() {
        assert_eq!(landing(Zone::Top, false), (Place::Before, false));
        assert_eq!(landing(Zone::Top, true), (Place::Before, false));
        assert_eq!(landing(Zone::Middle, true), (Place::Inside, false));
        assert_eq!(landing(Zone::Bottom, false), (Place::After, false));
        assert_eq!(landing(Zone::Bottom, true), (Place::Before, true));
    }

    #[test]
    fn the_tree_scrolls_only_near_its_top_and_its_bottom() {
        assert_eq!(scroll_toward(10, 500, 22), Some(false));
        assert_eq!(scroll_toward(-300, 500, 22), Some(false));
        assert_eq!(scroll_toward(21, 500, 22), Some(false));
        assert_eq!(scroll_toward(22, 500, 22), None);
        assert_eq!(scroll_toward(250, 500, 22), None);
        assert_eq!(scroll_toward(478, 500, 22), None);
        assert_eq!(scroll_toward(479, 500, 22), Some(true));
        assert_eq!(scroll_toward(900, 500, 22), Some(true));
    }
}
