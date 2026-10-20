//! The documents the engine thread keeps besides the one it serves (the other tabs of the window): what each costs while it waits, how
//! many fit, and which ones to let go of when a new one needs room. Only the choice is plain arithmetic (and tested as such); the
//! thread in `mod.rs` and `server.rs` does the rest.
//!
//! A document that waits holds its file, the objects of the edits not saved, the history that undoes them and the password it was opened
//! with, and nothing else: the drawing's caches, the text of the pages read and the decoded object streams went when it stopped being
//! served. A document with no edits that are not saved and no password can be let go of altogether (the window opens it again from the
//! disk when its tab is wanted); the others stay, whatever they cost.

use super::OpenDoc;
use super::editing::Session;

/// What a document costs besides its bytes and what it holds of edits, while it waits: the room after the file that a save appends to
/// (`view::SPARE_AFTER_FILE`, 1 MiB) and the page tree and the like (a quarter of a MiB), and, for each object the file has, the
/// cross-reference slot and the place of the stream end (together a little over 16 bytes).
const PARKED_FIXED: u64 = (1 << 20) + (256 << 10);
const PER_OBJECT: u64 = 16;

/// What a document of `objects` objects costs besides its bytes, while it waits.
fn base_for(objects: u64) -> u64 {
    PARKED_FIXED.saturating_add(PER_OBJECT.saturating_mul(objects))
}

/// What a file of `size` bytes that has not been read yet will cost besides its bytes, at a guess of an object to a hundred bytes.
pub(crate) fn guess_for(size: u64) -> u64 {
    base_for(size / 100)
}

/// A document that waits.
pub(crate) struct Parked {
    pub(crate) open: OpenDoc,
    pub(crate) session: Session,
}

impl Parked {
    /// What it costs while it waits, in bytes.
    fn bytes(&self) -> u64 {
        let document = &self.open.document;
        let size = document.data().len() as u64;
        // (The number of objects comes from the file, and from the trailer, which can say anything: it is held to what a file of this size can have.)
        let objects = u64::from(document.next_free_number()).min(size / 8 + 4096);
        size.saturating_add(document.overlay_bytes() as u64).saturating_add(self.session.held_bytes() as u64).saturating_add(base_for(objects))
    }

    /// May the engine let go of it? Not with edits that are not saved, text recognised for a file that cannot be written to (kept in
    /// memory only), or a password (the window would have to ask for it again).
    fn droppable(&self) -> bool {
        !self.session.dirty() && self.session.password().is_empty() && !self.session.keeps_text()
    }
}

/// The documents that wait, the least recently served first.
#[derive(Default)]
pub(crate) struct Store {
    docs: Vec<Parked>,
}

impl Store {
    /// Put a document that has just stopped being served among those that wait, as the most recently served (its decoded object streams are
    /// let go of).
    pub(crate) fn park(&mut self, open: OpenDoc, session: Session) {
        open.document.set_object_stream_cache_limit(0);
        self.remove(open.doc_id);
        self.docs.push(Parked { open, session });
    }

    pub(crate) fn take(&mut self, doc: u64) -> Option<Parked> {
        let i = self.docs.iter().position(|p| p.open.doc_id == doc)?;
        Some(self.docs.remove(i))
    }

    pub(crate) fn remove(&mut self, doc: u64) {
        self.docs.retain(|p| p.open.doc_id != doc);
    }

    pub(crate) fn clear(&mut self) {
        self.docs.clear();
    }

    /// What they all cost.
    pub(crate) fn bytes(&self) -> u64 {
        self.docs.iter().map(Parked::bytes).fold(0, u64::saturating_add)
    }

    /// The documents that may be let go of, the least recently served first: (document, what it costs).
    pub(crate) fn droppable(&self) -> Vec<(u64, u64)> {
        self.docs.iter().filter(|p| p.droppable()).map(|p| (p.open.doc_id, p.bytes())).collect()
    }
}

/// How many bytes must be freed before a document of `new` bytes can be held with the `held` bytes already, so that all of them together
/// and the least a document being served needs (`floor`, `plan::ACTIVE_FLOOR`) fit `total`: the floor is kept for whichever of them is
/// served next. 0: it fits as it is.
pub(crate) fn short_by(total: u64, held: u64, new: u64, floor: u64) -> u64 {
    held.saturating_add(new).saturating_add(floor).saturating_sub(total)
}

/// The documents to let go of to free `need` bytes, from a list of (document, bytes) with the least recently served first: as many from the
/// front as it takes. `None`: all of them together would not free enough. Nothing is chosen when `need` is 0.
pub(crate) fn pick_drops(droppable: &[(u64, u64)], need: u64) -> Option<Vec<u64>> {
    let (mut freed, mut chosen) = (0u64, Vec::new());
    for &(doc, bytes) in droppable {
        if freed >= need {
            break;
        }
        freed = freed.saturating_add(bytes);
        chosen.push(doc);
    }
    (freed >= need).then_some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_fits_needs_nothing_to_be_freed() {
        // 200 MB in all, 60 MB held, a new file of 10 MB, a floor of 70 MB: fits.
        assert_eq!(short_by(200, 60, 10, 70), 0);
        assert_eq!(short_by(200, 120, 10, 70), 0);
        assert_eq!(short_by(200, 121, 10, 70), 1);
        assert_eq!(short_by(200, 190, 30, 70), 90);
        // Sums that would overflow are held to the largest number.
        assert_eq!(short_by(200, u64::MAX, u64::MAX, u64::MAX), u64::MAX - 200);
    }

    #[test]
    fn a_waiting_document_costs_its_fixed_part_and_so_much_for_each_object() {
        assert_eq!(base_for(0), PARKED_FIXED);
        assert_eq!(base_for(1000), PARKED_FIXED + 16_000);
        assert_eq!(guess_for(0), PARKED_FIXED);
        assert_eq!(guess_for(100_000_000), PARKED_FIXED + 16_000_000);
        assert_eq!(base_for(u64::MAX), u64::MAX);
        // A file with more objects costs more than one with fewer, of the same size once read.
        assert!(base_for(2_000_000) > base_for(2_000) + 30_000_000);
    }

    #[test]
    fn the_least_recently_served_goes_first_and_only_as_many_as_it_takes() {
        let list = [(2, 10), (3, 10), (1, 10)];
        assert_eq!(pick_drops(&list, 0), Some(vec![]));
        assert_eq!(pick_drops(&list, 1), Some(vec![2]));
        assert_eq!(pick_drops(&list, 10), Some(vec![2]));
        assert_eq!(pick_drops(&list, 11), Some(vec![2, 3]));
        assert_eq!(pick_drops(&list, 30), Some(vec![2, 3, 1]));
        assert_eq!(pick_drops(&list, 31), None);
        assert_eq!(pick_drops(&[], 1), None);
        assert_eq!(pick_drops(&[], 0), Some(vec![]));
        // A big one further back is taken when the small ones in front are not enough.
        assert_eq!(pick_drops(&[(1, 1), (2, 100)], 50), Some(vec![1, 2]));
    }
}
