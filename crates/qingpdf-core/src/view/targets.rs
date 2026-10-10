//! Where an outline item or a link leads (3d-2): an explicit destination (12.3.2.2) turned into a page of the open document
//! and a place down it, with the named destinations of the file read once and only when something asks.

use std::cell::OnceCell;
use std::collections::HashMap;

use crate::dests::NamedDests;
use crate::document::{Document, Page};
use crate::object::Object;
use crate::render::work::{Work, cost};
use crate::text::Shown;

/// The named destinations of a document, read the first time they are needed. A file whose names cannot be read (or ask
/// for more than the limit) has none: the items that need them lead nowhere.
pub(super) struct Names<'a> {
    doc: &'a Document,
    cell: OnceCell<Option<NamedDests>>,
}

impl<'a> Names<'a> {
    pub(super) fn new(doc: &'a Document) -> Names<'a> {
        Names { doc, cell: OnceCell::new() }
    }

    /// The names, read now if they have not been. Reading them is charged to `work` by the entries and nodes of the tree as
    /// it goes and stops when `work` is cancelled (nothing is kept then, and the next ask reads them again); a file whose
    /// names run `work` out has none, for good.
    pub(super) fn get(&self, work: &Work) -> Option<&NamedDests> {
        if let Some(known) = self.cell.get() {
            return known.as_ref();
        }
        // A meter that was spent before this started says nothing about the file.
        let fresh = !work.is_over();
        let names = match NamedDests::load_with(self.doc, &|steps| work.charge(steps as f64 * cost::NAMED_DEST)) {
            Ok(n) => Some(n),
            Err(_) if work.was_cancelled() || !fresh => return None,
            Err(_) => None,
        };
        self.cell.get_or_init(|| names).as_ref()
    }
}

/// The page (from 0) a destination points at and how far down it, in points from the top of the page as it is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Target {
    pub page: u32,
    pub y: Option<f32>,
}

/// The target of an explicit destination array. `index` maps the object number of a page to its place in the document. A
/// destination that does not name one of the pages, or whose page is given as a number (a destination in another file),
/// leads nowhere.
pub(super) fn target(array: &[Object], pages: &[Page], index: &HashMap<u32, u32>) -> Option<Target> {
    let Some(Object::Ref(r)) = array.first() else { return None };
    let at = *index.get(&r.num)?;
    let page = pages.get(at as usize)?;
    let number = |i: usize| array.get(i).and_then(Object::as_f64).filter(|v| v.is_finite());
    // Table 151: [page /XYZ left top zoom], [page /FitH top], [page /FitR left bottom right top], [page /FitBH top].
    let (left, top) = match array.get(1).and_then(Object::as_name).map(|n| n.as_bytes()) {
        Some(b"XYZ") => (number(2), number(3)),
        Some(b"FitH" | b"FitBH") => (None, number(2)),
        Some(b"FitR") => (number(2), number(5)),
        _ => (None, None),
    };
    let y = top.map(|t| {
        let (_, y) = Shown::of(page).point(left, t);
        y.clamp(0.0, 1.0e6) as f32
    });
    Some(Target { page: at, y })
}

/// Object number of each page to its place.
pub(super) fn page_index(pages: &[Page]) -> HashMap<u32, u32> {
    let mut index = HashMap::with_capacity(pages.len());
    for (i, p) in pages.iter().enumerate() {
        index.entry(p.obj_ref.num).or_insert(u32::try_from(i).unwrap_or(u32::MAX));
    }
    index
}
