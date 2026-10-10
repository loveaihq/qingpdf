//! The results of a search (3d-2): the places found, page by page as the engine tells them, which one is the current one, and how to
//! move to the next and the previous. Plain logic; the search itself runs in the engine.

use std::collections::BTreeMap;

use qingpdf_core::view::SearchHit;

/// A place found: the page, and which of the page's hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitRef {
    pub page: u32,
    pub index: usize,
}

pub struct SearchState {
    /// The engine's request for this search.
    pub id: u64,
    pub query: String,
    /// The places found, by page.
    hits: BTreeMap<u32, Vec<SearchHit>>,
    total: usize,
    pub current: Option<HitRef>,
    /// The engine is still looking.
    pub running: bool,
    pub pages_done: u32,
    pub pages_total: u32,
    /// It stopped at the most places the reader reports.
    pub limit: bool,
    pub skipped: u32,
}

impl SearchState {
    pub fn new(id: u64, query: &str) -> SearchState {
        SearchState { id, query: query.to_string(), hits: BTreeMap::new(), total: 0, current: None, running: true, pages_done: 0, pages_total: 0, limit: false, skipped: 0 }
    }

    /// The hits the engine found on `page`. Returns `true` if this is the first place found (and so it becomes the current one).
    pub fn add(&mut self, page: u32, hits: Vec<SearchHit>) -> bool {
        if hits.is_empty() {
            return false;
        }
        self.total += hits.len();
        let first = self.current.is_none();
        self.hits.entry(page).or_default().extend(hits);
        if first {
            self.current = Some(HitRef { page, index: 0 });
        }
        first
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn page_hits(&self, page: u32) -> &[SearchHit] {
        self.hits.get(&page).map_or(&[], Vec::as_slice)
    }

    pub fn current_hit(&self) -> Option<&SearchHit> {
        let c = self.current?;
        self.hits.get(&c.page)?.get(c.index)
    }

    /// The place's number among all found (from 1), by page order.
    pub fn number_of(&self, at: HitRef) -> usize {
        let before: usize = self.hits.range(..at.page).map(|(_, v)| v.len()).sum();
        before + at.index + 1
    }

    /// Go to the next place after the current one (the first again after the last). `None` when there is none at all.
    pub fn next(&mut self) -> Option<HitRef> {
        let target = match self.current {
            None => self.first(),
            Some(c) => {
                let in_page = self.hits.get(&c.page).map_or(0, Vec::len);
                if c.index + 1 < in_page {
                    Some(HitRef { page: c.page, index: c.index + 1 })
                } else {
                    self.hits.range(c.page.saturating_add(1)..).next().map(|(&page, _)| HitRef { page, index: 0 }).or_else(|| self.first())
                }
            }
        };
        self.current = target;
        target
    }

    /// Go to the place before the current one (the last again before the first).
    pub fn prev(&mut self) -> Option<HitRef> {
        let target = match self.current {
            None => self.last(),
            Some(c) => {
                if c.index > 0 {
                    Some(HitRef { page: c.page, index: c.index - 1 })
                } else {
                    self.hits.range(..c.page).next_back().map(|(&page, v)| HitRef { page, index: v.len() - 1 }).or_else(|| self.last())
                }
            }
        };
        self.current = target;
        target
    }

    fn first(&self) -> Option<HitRef> {
        self.hits.iter().next().map(|(&page, _)| HitRef { page, index: 0 })
    }

    fn last(&self) -> Option<HitRef> {
        self.hits.iter().next_back().map(|(&page, v)| HitRef { page, index: v.len() - 1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(n: usize) -> Vec<SearchHit> {
        (0..n).map(|i| SearchHit { start: i as u32, len: 1, rects: vec![[0.0, 0.0, 1.0, 1.0]] }).collect()
    }

    #[test]
    fn the_first_place_found_is_the_current_one_whichever_page_it_is_on() {
        let mut s = SearchState::new(1, "x");
        assert!(!s.add(4, Vec::new()));
        assert!(s.add(7, hits(2)));
        assert!(!s.add(2, hits(1)));
        assert_eq!(s.current, Some(HitRef { page: 7, index: 0 }));
        assert_eq!(s.total(), 3);
        assert_eq!(s.page_hits(7).len(), 2);
        assert!(s.page_hits(3).is_empty());
        assert!(s.current_hit().is_some());
    }

    #[test]
    fn next_and_previous_go_round_in_page_order() {
        let mut s = SearchState::new(1, "x");
        s.add(7, hits(2));
        s.add(2, hits(1));
        s.add(9, hits(1));
        // Page order: (2,0) (7,0) (7,1) (9,0). The current one is the first found: (7,0).
        assert_eq!(s.number_of(s.current.expect("current")), 2);
        let order: Vec<(u32, usize)> = (0..5).filter_map(|_| s.next()).map(|h| (h.page, h.index)).collect();
        assert_eq!(order, [(7, 1), (9, 0), (2, 0), (7, 0), (7, 1)]);
        let back: Vec<(u32, usize)> = (0..5).filter_map(|_| s.prev()).map(|h| (h.page, h.index)).collect();
        assert_eq!(back, [(7, 0), (2, 0), (9, 0), (7, 1), (7, 0)]);
        assert_eq!(s.number_of(HitRef { page: 9, index: 0 }), 4);
    }

    #[test]
    fn with_nothing_found_there_is_nowhere_to_go() {
        let mut s = SearchState::new(1, "x");
        assert_eq!((s.next(), s.prev(), s.current), (None, None, None));
        assert!(s.current_hit().is_none());
    }
}
