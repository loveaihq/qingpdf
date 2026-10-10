//! The bitmaps the window keeps: drawn pieces of pages, held to a number of bytes. When a new one does not fit, the
//! ones on the pages farthest from the pages in view go first.

use std::collections::HashMap;

/// What a drawn piece is: which page, turned how far, at what scale, which tile of it.
/// (`scale` is the scale in thousandths of a pixel a point, so that equal scales are equal keys.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub page: u32,
    pub rotation: u16,
    pub scale: u64,
    pub tx: u32,
    pub ty: u32,
}

/// A drawn piece. `x`, `y` are where it starts in the page's pixels at its own scale, `page_w`, `page_h` the size of
/// the whole page there, so that it can be put on the page at another scale.
pub struct Entry {
    pub key: Key,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    /// A small picture of the whole page, drawn to be shown until the real one is there.
    pub preview: bool,
    pub x: i64,
    pub y: i64,
    pub page_w: i64,
    pub page_h: i64,
}

impl Entry {
    pub fn bytes(&self) -> u64 {
        self.bgra.len() as u64
    }
}

/// What is in view, for deciding what to keep.
#[derive(Clone, Copy, Debug)]
pub struct Focus {
    pub first: u32,
    pub last: u32,
    pub rotation: u16,
    pub scale: u64,
}

impl Focus {
    fn distance(&self, page: u32) -> u32 {
        if page < self.first {
            self.first - page
        } else {
            page.saturating_sub(self.last)
        }
    }
}

pub struct BitmapCache {
    map: HashMap<Key, Entry>,
    bytes: u64,
    budget: u64,
}

impl BitmapCache {
    pub fn new(budget: u64) -> BitmapCache {
        BitmapCache { map: HashMap::new(), bytes: 0, budget }
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    #[cfg(test)]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[cfg(test)]
    pub fn get(&self, key: &Key) -> Option<&Entry> {
        self.map.get(key)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.map.contains_key(key)
    }

    /// All the entries of one page.
    pub fn page(&self, page: u32) -> impl Iterator<Item = &Entry> {
        self.map.values().filter(move |e| e.key.page == page)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    /// Put an entry in (replacing one with the same key) and make room: the keys of the entries dropped are returned.
    pub fn insert(&mut self, entry: Entry, focus: &Focus) -> Vec<Key> {
        let added = entry.bytes();
        if let Some(old) = self.map.insert(entry.key, entry) {
            self.bytes = self.bytes.saturating_sub(old.bytes());
        }
        self.bytes = self.bytes.saturating_add(added);
        self.make_room(focus)
    }

    /// Hold the cache to `budget` bytes from now on.
    #[cfg(test)]
    pub fn set_budget(&mut self, budget: u64, focus: &Focus) -> Vec<Key> {
        self.budget = budget;
        self.make_room(focus)
    }

    /// Drop the pieces of a page that are not at `rotation` and `scale` and are not previews (they were drawn for a
    /// zoom or a turn that is over; the page has been drawn again).
    pub fn drop_stale(&mut self, page: u32, rotation: u16, scale: u64) {
        let gone: Vec<Key> = self.map.values().filter(|e| e.key.page == page && !e.preview && (e.key.rotation != rotation || e.key.scale != scale)).map(|e| e.key).collect();
        for k in gone {
            self.remove(&k);
        }
    }

    /// Drop every piece of a page (it has changed: what was drawn is out of date).
    pub fn remove_page(&mut self, page: u32) {
        let gone: Vec<Key> = self.map.values().filter(|e| e.key.page == page).map(|e| e.key).collect();
        for k in gone {
            self.remove(&k);
        }
    }

    pub fn remove(&mut self, key: &Key) {
        if let Some(old) = self.map.remove(key) {
            self.bytes = self.bytes.saturating_sub(old.bytes());
        }
    }

    /// How soon an entry should go: the farther its page from the pages in view, the sooner; at the same distance,
    /// a piece drawn for another zoom first, then a sharp piece, then a preview (the cheapest to keep); then a page
    /// behind the view before one ahead of it (people read forward).
    fn rank(e: &Entry, focus: &Focus) -> (u32, u8, u8) {
        let class = if e.preview {
            0
        } else if e.key.rotation == focus.rotation && e.key.scale == focus.scale {
            1
        } else {
            2
        };
        (focus.distance(e.key.page), class, u8::from(e.key.page < focus.first))
    }

    fn make_room(&mut self, focus: &Focus) -> Vec<Key> {
        let mut dropped = Vec::new();
        while self.bytes > self.budget {
            let Some((worst, key)) = self.map.values().map(|e| (Self::rank(e, focus), e.key)).max_by_key(|&(rank, key)| (rank, key.page, key.tx, key.ty)) else { break };
            // What is in view, at the scale it is drawn at (`focus.scale`), stays even if the cache is over. The scheduler
            // keeps that from being more than the budget: when the pieces in view would not fit it asks for them at a smaller
            // scale (`sched::draw_scale`), and that smaller scale is the focus's, so they stay and are stretched.
            if worst.0 == 0 && worst.1 <= 1 {
                break;
            }
            self.remove(&key);
            dropped.push(key);
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(page: u32, scale: u64, bytes: usize, preview: bool) -> Entry {
        Entry { key: Key { page, rotation: 0, scale, tx: 0, ty: 0 }, width: 1, height: bytes as u32 / 4, bgra: vec![0; bytes], preview, x: 0, y: 0, page_w: 1, page_h: 1 }
    }

    fn focus(first: u32, last: u32) -> Focus {
        Focus { first, last, rotation: 0, scale: 1000 }
    }

    #[test]
    fn the_farthest_pages_go_first() {
        let mut c = BitmapCache::new(1000);
        for p in [5, 6, 7, 8, 9] {
            assert!(c.insert(entry(p, 1000, 200, false), &focus(7, 7)).is_empty());
        }
        assert_eq!(c.bytes(), 1000);
        // Another: page 5 and 9 are both two away; the higher page goes first of two equals.
        let dropped = c.insert(entry(10, 1000, 200, false), &focus(7, 7));
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].page, 10);
        assert_eq!(c.len(), 5);
        // The view moves down: page 5 is the farthest now.
        let dropped = c.insert(entry(11, 1000, 200, false), &focus(9, 9));
        assert_eq!(dropped.iter().map(|k| k.page).collect::<Vec<_>>(), vec![5]);
        assert_eq!(c.bytes(), 1000);
    }

    #[test]
    fn at_the_same_distance_other_zooms_go_before_sharp_and_sharp_before_previews() {
        let mut c = BitmapCache::new(600);
        c.insert(entry(4, 1000, 200, false), &focus(5, 5));
        c.insert(entry(4, 500, 200, false), &focus(5, 5));
        c.insert(entry(4, 100, 200, true), &focus(5, 5));
        assert_eq!(c.bytes(), 600);
        let dropped = c.insert(entry(6, 1000, 100, false), &focus(5, 5));
        // Pages 4 and 6 are one away; the drop is the other zoom's piece of page 4 (scale 500).
        assert_eq!(dropped, vec![Key { page: 4, rotation: 0, scale: 500, tx: 0, ty: 0 }]);
        let dropped = c.insert(entry(6, 1000, 400, false), &focus(5, 5));
        // Over again: the sharp pieces of 4 and 6 are alike but 4 is behind the view; the preview of page 4 stays.
        assert_eq!(dropped, vec![Key { page: 4, rotation: 0, scale: 1000, tx: 0, ty: 0 }]);
        assert!(c.get(&Key { page: 4, rotation: 0, scale: 100, tx: 0, ty: 0 }).is_some(), "the preview stays longest");
    }

    #[test]
    fn what_is_in_view_is_not_dropped_when_it_does_not_fit() {
        let mut c = BitmapCache::new(300);
        let f = focus(2, 3);
        c.insert(entry(2, 1000, 200, false), &f);
        let dropped = c.insert(entry(3, 1000, 200, false), &f);
        assert!(dropped.is_empty());
        assert_eq!(c.bytes(), 400);
        // But a piece for another zoom on a page in view goes.
        let dropped = c.insert(entry(2, 500, 100, false), &f);
        assert_eq!(dropped, vec![Key { page: 2, rotation: 0, scale: 500, tx: 0, ty: 0 }]);
    }

    #[test]
    fn replacing_an_entry_keeps_the_count_right_and_stale_pieces_can_be_dropped() {
        let mut c = BitmapCache::new(10_000);
        let f = focus(0, 0);
        c.insert(entry(0, 1000, 400, false), &f);
        c.insert(entry(0, 1000, 100, false), &f);
        assert_eq!((c.len(), c.bytes()), (1, 100));
        c.insert(entry(0, 500, 300, false), &f);
        c.insert(entry(0, 50, 50, true), &f);
        c.drop_stale(0, 0, 1000);
        assert_eq!((c.len(), c.bytes()), (2, 150), "the sharp piece at 1000 and the preview stay");
        c.set_budget(120, &focus(9, 9));
        assert!(c.bytes() <= 120);
        c.clear();
        assert!(c.is_empty() && c.bytes() == 0);
    }
}
