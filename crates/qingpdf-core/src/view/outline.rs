//! The bookmarks of a file (3d-2): `/Outlines` (12.3.3) read into a flat list in display order, each item with its depth,
//! its title decoded and the page and place it leads to.
//!
//! The file is not trusted: the number of items and their depth are capped, a chain that comes back to an item it has been
//! at ends there, the titles are cut, and every item is charged to the work meter by what reading it cost.

use std::collections::{HashMap, HashSet};

use crate::dests;
use crate::document::{Document, Page};
use crate::info::decode_text_string;
use crate::object::{ObjRef, Object};
use crate::render::work::{Work, cost};

use super::targets::{Names, target};

/// Most items read; the rest are left out.
pub const MAX_OUTLINE_ITEMS: usize = 20_000;
/// Deepest level read (the top level is 0); deeper ones are left out.
pub const MAX_OUTLINE_DEPTH: usize = 32;
/// Most characters of a title kept.
pub const MAX_TITLE_CHARS: usize = 300;
/// Bytes of a title that are looked at (a title is cut to [`MAX_TITLE_CHARS`] after decoding).
const MAX_TITLE_BYTES: usize = 4 * MAX_TITLE_CHARS;

/// One bookmark.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    /// The title as text (empty if the item has none).
    pub title: String,
    /// 0 for the top level; an item is a child of the nearest item before it that is one level less deep.
    pub depth: u32,
    /// The page it leads to, from 0, or `None` if it leads nowhere that could be found.
    pub page: Option<u32>,
    /// How far down that page, in points from the top of the page as it is shown, when the destination says.
    pub y: Option<f32>,
    /// The file shows the item with its children open.
    pub open: bool,
}

/// What [`read`] found.
pub(super) struct Outline {
    pub items: Vec<OutlineItem>,
    /// Items were left out: too many, too deep, or the work ran out.
    pub truncated: bool,
}

/// The bookmarks of `doc`. Stops early (with what was read) when `work` is spent or cancelled.
pub(super) fn read(doc: &Document, pages: &[Page], index: &HashMap<u32, u32>, names: &Names<'_>, work: &Work) -> Outline {
    let mut out = Outline { items: Vec::new(), truncated: false };
    let Ok(catalog) = doc.catalog() else { return out };
    let Some(root) = catalog.get("Outlines").and_then(|o| doc.resolve(o).ok()) else { return out };
    let Some(root) = root.as_dict() else { return out };
    let Some(Object::Ref(first)) = root.get("First") else { return out };
    // The object numbers of the items read: an item met twice (a chain that loops, a child that is also a sibling) is not
    // followed again.
    let mut seen: HashSet<u32> = HashSet::new();
    if let Some(Object::Ref(r)) = catalog.get("Outlines") {
        seen.insert(r.num);
    }
    // The chains being walked, innermost last: the next item of each, and its depth.
    let mut stack: Vec<(Option<ObjRef>, usize)> = vec![(Some(*first), 0)];
    while let Some(top) = stack.last_mut() {
        let depth = top.1;
        let Some(r) = top.0.take() else {
            stack.pop();
            continue;
        };
        if out.items.len() >= MAX_OUTLINE_ITEMS {
            out.truncated = true;
            break;
        }
        if !seen.insert(r.num) {
            continue;
        }
        let Ok(Object::Dict(d)) = doc.get(r) else { continue };
        if !work.charge(cost::OUTLINE_ITEM + d.approx_size() as f64 * cost::RESOLVE_BYTE) {
            out.truncated = true;
            break;
        }
        top.0 = match d.get("Next") {
            Some(Object::Ref(next)) => Some(*next),
            _ => None,
        };
        // Through the meter: a title that is one big object shared by every item is read from the file once and charged
        // by its size, and each use after that is charged a little (review of 3d-2).
        let title = match d.get("Title").and_then(|t| work.read(doc, t)).as_deref() {
            Some(Object::String(s)) => {
                let bytes = s.bytes.get(..s.bytes.len().min(MAX_TITLE_BYTES)).unwrap_or(&[]);
                decode_text_string(bytes).chars().take(MAX_TITLE_CHARS).collect()
            }
            _ => String::new(),
        };
        let found = names.get(work).and_then(|n| dests::item_array(doc, &d, n).ok().flatten()).and_then(|a| target(&a, pages, index));
        out.items.push(OutlineItem {
            title,
            depth: u32::try_from(depth).unwrap_or(0),
            page: found.map(|t| t.page),
            y: found.and_then(|t| t.y),
            open: d.get("Count").and_then(Object::as_int).is_some_and(|c| c > 0),
        });
        if let Some(Object::Ref(child)) = d.get("First") {
            if depth + 1 < MAX_OUTLINE_DEPTH {
                stack.push((Some(*child), depth + 1));
            } else {
                out.truncated = true;
            }
        }
    }
    out
}
