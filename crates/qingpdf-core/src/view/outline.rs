//! The bookmarks of a file (3d-2): `/Outlines` (12.3.3) read into a flat list in display order, each item with its depth,
//! its title decoded and the page and place it leads to.
//!
//! The file is not trusted: the number of items and their depth are capped, a chain that comes back to an item it has been
//! at ends there, the titles are cut, and every item is charged to the work meter by what reading it cost.
//!
//! The same list is what the bookmarks are edited as (2026-10-20): the window changes it (`bookmarks.rs`) and hands the whole of it
//! back (`edit::outline`), each item still knowing the object it was read from.


use crate::edit::Edit;
use crate::error::Result;
use crate::idmap::{IdMap, IdSet};
use crate::dests;
use crate::document::{Document, Page};
use crate::info::decode_text_string;
use crate::object::{Dict, ObjRef, Object};
use crate::render::work::{Work, cost};

use super::targets::{Names, page_index, target};
use super::Command;

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
    /// The object the item is in the file (0: a bookmark that is not in the file yet).
    pub id: u32,
    /// The item does something other than go to a place (a link, a named action ...): when it is written its action stays as it is.
    pub action: bool,
    /// When it is written, `page`, `x` and `y` are its destination (a bookmark that was made or set to a place in the window); an item
    /// that is not `placed` keeps the destination it has in the file. How far right, in points from the left of the page as shown.
    pub placed: bool,
    pub x: Option<f32>,
}

impl OutlineItem {
    /// A new bookmark that leads to `page` (from 0), at `x` and `y` of the page as shown, or to the top of the page.
    pub fn made(title: &str, depth: u32, page: Option<u32>, x: Option<f32>, y: Option<f32>) -> OutlineItem {
        OutlineItem { title: title.to_string(), depth, page, y, open: false, id: 0, action: false, placed: page.is_some(), x }
    }
}

/// What [`read`] found.
pub(super) struct Outline {
    pub items: Vec<OutlineItem>,
    /// Items were left out: too many, too deep, or the work ran out.
    pub truncated: bool,
}

/// Is this dictionary no bookmark? A hostile file can point `/First` or `/Next` at a page, a node of the page tree, the catalog or
/// an outline, or at some other object, to have it rewritten as a bookmark (its `/Parent`, `/Prev` ... put in it): such an object is left
/// out of the bookmarks, so it is neither shown nor ever written. An object is no bookmark when it is the catalog, says it is a page, a node of
/// the page tree, the catalog or an outline, or has a key of what is not a bookmark (the kids of a node, the subtype of an annotation, a font or
/// a picture, the box and the content of a page).
fn not_a_bookmark(r: ObjRef, d: &Dict, catalog: Option<ObjRef>) -> bool {
    const TYPES: [&[u8]; 4] = [b"Page", b"Pages", b"Catalog", b"Outlines"];
    const KEYS: [&str; 8] = ["Kids", "Subtype", "MediaBox", "Contents", "Resources", "Annots", "Pages", "Outlines"];
    Some(r) == catalog || matches!(d.get("Type"), Some(Object::Name(n)) if TYPES.contains(&n.as_bytes())) || KEYS.iter().any(|k| d.contains_key(k))
}

/// Go through the items of the file's bookmarks in display order, calling `each` with the item's reference, its dictionary and its
/// depth. Each item is looked at once; at most [`MAX_OUTLINE_ITEMS`] of them, [`MAX_OUTLINE_DEPTH`] deep. Returns whether items were left
/// out (too many, too deep, or `work` ran out).
pub(crate) fn walk(doc: &Document, work: &Work, mut each: impl FnMut(ObjRef, &Dict, usize)) -> bool {
    let Ok(catalog) = doc.catalog() else { return false };
    let Some(root) = catalog.get("Outlines").and_then(|o| doc.resolve(o).ok()) else { return false };
    let Some(root) = root.as_dict() else { return false };
    let Some(Object::Ref(first)) = root.get("First") else { return false };
    // The object numbers of the items read: an item met twice (a chain that loops, a child that is also a sibling) is not
    // followed again.
    let mut seen: IdSet<u32> = IdSet::new();
    if let Some(Object::Ref(r)) = catalog.get("Outlines") {
        seen.insert(r.num);
    }
    let catalog_ref = doc.trailer().get("Root").and_then(Object::as_obj_ref);
    let (mut count, mut truncated) = (0usize, false);
    // The chains being walked, innermost last: the next item of each, and its depth.
    let mut stack: Vec<(Option<ObjRef>, usize)> = vec![(Some(*first), 0)];
    while let Some(top) = stack.last_mut() {
        let depth = top.1;
        let Some(r) = top.0.take() else {
            stack.pop();
            continue;
        };
        if count >= MAX_OUTLINE_ITEMS {
            truncated = true;
            break;
        }
        if !seen.insert(r.num) {
            continue;
        }
        let Ok(Object::Dict(d)) = doc.get(r) else { continue };
        if !work.charge(cost::OUTLINE_ITEM + d.approx_size() as f64 * cost::RESOLVE_BYTE) {
            truncated = true;
            break;
        }
        top.0 = match d.get("Next") {
            Some(Object::Ref(next)) => Some(*next),
            _ => None,
        };
        if not_a_bookmark(r, &d, catalog_ref) {
            continue;
        }
        count += 1;
        each(r, &d, depth);
        if let Some(Object::Ref(child)) = d.get("First") {
            if depth + 1 < MAX_OUTLINE_DEPTH {
                stack.push((Some(*child), depth + 1));
            } else {
                truncated = true;
            }
        }
    }
    truncated
}

/// A command that waited behind a rewrite of the whole file, which numbers its objects afresh: a change to the bookmarks names its items by
/// object number, so they are made the numbers of the items at the same places of the new file's bookmarks (the old and the new are walked in
/// the same order). A command that is anything else, and an item that is not one of the old file's, are left as they are (the change then
/// answers that the list is not the file's).
pub(super) fn renumbered(command: Command, old: &Document, new: &Document) -> Command {
    let Command::Edit { doc, id, edit: Edit::Outline(mut items) } = command else { return command };
    let numbers = |d: &Document| {
        let mut all = Vec::new();
        walk(d, &Work::new(), |r, _, _| all.push(r.num));
        all
    };
    let (was, is) = (numbers(old), numbers(new));
    if was.len() == is.len() {
        let map: IdMap<u32, u32> = was.into_iter().zip(is).collect();
        for item in &mut items {
            if let Some(&n) = map.get(&item.id) {
                item.id = n;
            }
        }
    }
    Command::Edit { doc, id, edit: Edit::Outline(items) }
}

/// The bookmarks of a document as the reader shows them, and whether some were left out (too many, too deep).
pub fn read_bookmarks(doc: &Document) -> Result<(Vec<OutlineItem>, bool)> {
    let pages = doc.pages()?;
    let index = page_index(&pages);
    let found = read(doc, &pages, &index, &Names::new(doc), &Work::new());
    Ok((found.items, found.truncated))
}

/// The bookmarks of `doc`. Stops early (with what was read) when `work` is spent or cancelled.
pub(super) fn read(doc: &Document, pages: &[Page], index: &IdMap<u32, u32>, names: &Names<'_>, work: &Work) -> Outline {
    let mut items = Vec::new();
    let truncated = walk(doc, work, |r, d, depth| {
        // Through the meter: a title that is one big object shared by every item is read from the file once and charged
        // by its size, and each use after that is charged a little (review of 3d-2).
        let title = match d.get("Title").and_then(|t| work.read(doc, t)).as_deref() {
            Some(Object::String(s)) => {
                let bytes = s.bytes.get(..s.bytes.len().min(MAX_TITLE_BYTES)).unwrap_or(&[]);
                decode_text_string(bytes).chars().take(MAX_TITLE_CHARS).collect()
            }
            _ => String::new(),
        };
        let found = names.get(work).and_then(|n| dests::item_array(doc, d, n).ok().flatten()).and_then(|a| target(&a, pages, index));
        // An action that is not a go-to (a link, a named action, a script ...) is left as it is when the item is written.
        let action = d.get("A").and_then(|a| doc.resolve(a).ok()).as_ref().and_then(Object::as_dict).is_some_and(|a| !matches!(a.get("S"), Some(Object::Name(n)) if n == "GoTo"));
        items.push(OutlineItem {
            title,
            depth: u32::try_from(depth).unwrap_or(0),
            page: found.map(|t| t.page),
            y: found.and_then(|t| t.y),
            open: d.get("Count").and_then(Object::as_int).is_some_and(|c| c > 0),
            id: r.num,
            action,
            placed: false,
            x: None,
        });
    });
    Outline { items, truncated }
}
