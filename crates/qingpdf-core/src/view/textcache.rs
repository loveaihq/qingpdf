//! The characters of the pages read lately (3d-2 review): reading a page's text costs far more than searching it, and a
//! search, a selection and a copy all want the same pages, so what was read is kept, in a bounded number of bytes, for the
//! next to use.
//!
//! Two kinds of use differ. A page someone is working with (the boxes for a selection) takes the room of the page used
//! longest ago. A page read in passing by a pass over many (a search, a copy) is kept only while there is room: a pass over
//! more pages than fit would otherwise throw out each page just before the next pass wanted it, and the cache would never
//! be hit; this way the next pass finds the first pages that fitted.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::text::PageChars;

/// What one box and a slot of the map take besides the text itself.
const BOX_BYTES: usize = std::mem::size_of::<[f32; 4]>();
const SLOT_BYTES: usize = 96;

struct Slot {
    chars: Rc<PageChars>,
    weight: usize,
    used: u64,
}

pub(super) struct TextCache {
    budget: usize,
    bytes: usize,
    tick: u64,
    map: HashMap<u32, Slot>,
    /// Pages by when they were last used (smallest first).
    order: BTreeMap<u64, u32>,
}

/// The bytes a page's characters take in the cache.
fn weight(chars: &PageChars) -> usize {
    chars.text.len().saturating_add(chars.boxes.len().saturating_mul(BOX_BYTES)).saturating_add(SLOT_BYTES)
}

impl TextCache {
    pub(super) fn new(budget: u64) -> TextCache {
        TextCache { budget: usize::try_from(budget).unwrap_or(usize::MAX), bytes: 0, tick: 0, map: HashMap::new(), order: BTreeMap::new() }
    }

    /// The characters of `page` if they are kept; they count as used now.
    pub(super) fn get(&mut self, page: u32) -> Option<Rc<PageChars>> {
        self.tick += 1;
        let tick = self.tick;
        let slot = self.map.get_mut(&page)?;
        self.order.remove(&slot.used);
        slot.used = tick;
        self.order.insert(tick, page);
        Some(slot.chars.clone())
    }

    /// Keep the characters of `page` if they take no more than a quarter of the budget. `pass`: they were read in passing, by
    /// a pass over many pages; they are kept only if there is room without letting go of another page.
    pub(super) fn put(&mut self, page: u32, chars: Rc<PageChars>, pass: bool) {
        let w = weight(&chars);
        if w > self.budget / 4 {
            return;
        }
        self.forget(page);
        while self.bytes.saturating_add(w) > self.budget {
            if pass {
                return;
            }
            let Some((_, oldest)) = self.order.pop_first() else { return };
            if let Some(slot) = self.map.remove(&oldest) {
                self.bytes -= slot.weight;
            }
        }
        self.tick += 1;
        self.order.insert(self.tick, page);
        self.map.insert(page, Slot { chars, weight: w, used: self.tick });
        self.bytes += w;
    }

    fn forget(&mut self, page: u32) {
        if let Some(slot) = self.map.remove(&page) {
            self.order.remove(&slot.used);
            self.bytes -= slot.weight;
        }
    }

    /// Bytes kept now.
    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(chars: usize) -> Rc<PageChars> {
        Rc::new(PageChars { text: "a".repeat(chars), boxes: vec![[0.0; 4]; chars] })
    }

    #[test]
    fn it_never_holds_more_than_its_budget_and_drops_the_page_used_longest_ago() {
        let w = weight(&page(100));
        let mut c = TextCache::new((w * 4) as u64);
        for p in 0..4 {
            c.put(p, page(100), false);
        }
        assert_eq!((c.len(), c.bytes()), (4, w * 4));
        // Page 0 is used again; the next page takes the room of page 1.
        assert!(c.get(0).is_some());
        c.put(9, page(100), false);
        assert!(c.get(1).is_none() && c.get(0).is_some() && c.get(9).is_some());
        assert!(c.bytes() <= w * 4);
        // A page of more than a quarter of the budget is not kept at all.
        c.put(7, page(1000), false);
        assert!(c.get(7).is_none());
        // The same page again replaces the old one.
        c.put(9, page(100), false);
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn a_pass_over_more_pages_than_fit_keeps_the_first_ones_for_the_next_pass() {
        let w = weight(&page(100));
        let mut c = TextCache::new((w * 4) as u64);
        for p in 0..10 {
            if c.get(p).is_none() {
                c.put(p, page(100), true);
            }
        }
        let held: Vec<u32> = (0..10).filter(|&p| c.get(p).is_some()).collect();
        assert_eq!(held, [0, 1, 2, 3]);
        // A page someone works with does take the room of an older one.
        c.put(8, page(100), false);
        assert!(c.get(8).is_some() && c.len() == 4);
    }
}
