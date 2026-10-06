//! Named destinations (ISO 32000-1 12.3.2.3): the name tree `/Dests` in the
//! catalog's `/Names` dictionary (PDF 1.2) and the old `/Dests` dictionary of
//! the catalog (PDF 1.1), read into flat lists, and the helpers that turn a
//! destination into the page it points at.

use std::collections::{HashMap, HashSet};

use crate::document::Document;
use crate::object::{Dict, Name, Object, PdfString};

/// Most name tree nodes looked at, and most entries kept. Real files have
/// thousands; the limit only stops hostile ones.
const MAX_TREE_NODES: usize = 500_000;
pub(crate) const MAX_NAMED_DESTS: usize = 2_000_000;
/// Deepest name tree followed (7.9.6 trees are a few levels deep).
const MAX_TREE_DEPTH: usize = 64;

/// A name tree leaf holds at most this many entries when a tree is written.
const LEAF_SIZE: usize = 64;

/// What the name tree and the old dictionary name, in the order of the file.
/// The values are as they stand there: an array, a dictionary with a `/D`, or
/// a reference to one of those.
#[derive(Debug, Default)]
pub(crate) struct NamedDests {
    tree: Vec<(Vec<u8>, Object)>,
    dict: Vec<(Vec<u8>, Object)>,
    tree_index: HashMap<Vec<u8>, usize>,
    dict_index: HashMap<Vec<u8>, usize>,
}

impl NamedDests {
    /// Read both places from the catalog of `doc`. Whatever cannot be read is
    /// left out.
    pub(crate) fn load(doc: &Document) -> NamedDests {
        let mut out = NamedDests::default();
        let Ok(catalog) = doc.catalog() else {
            return out;
        };
        if let Some(Object::Dict(names)) = catalog.get("Names").and_then(|n| doc.resolve(n).ok())
            && let Some(root) = names.get("Dests")
        {
            out.tree = read_tree(doc, root);
        }
        if let Some(Object::Dict(dests)) = catalog.get("Dests").and_then(|d| doc.resolve(d).ok()) {
            for (key, value) in dests.iter() {
                if out.dict.len() >= MAX_NAMED_DESTS {
                    break;
                }
                out.dict.push((key.as_bytes().to_vec(), value.clone()));
            }
        }
        for (i, (key, _)) in out.tree.iter().enumerate() {
            out.tree_index.entry(key.clone()).or_insert(i);
        }
        for (i, (key, _)) in out.dict.iter().enumerate() {
            out.dict_index.entry(key.clone()).or_insert(i);
        }
        out
    }

    /// The entries of the name tree, in tree order.
    pub(crate) fn tree(&self) -> &[(Vec<u8>, Object)] {
        &self.tree
    }

    /// The entries of the old `/Dests` dictionary.
    pub(crate) fn dict(&self) -> &[(Vec<u8>, Object)] {
        &self.dict
    }

    /// The explicit destination array called `key`. A name object (`is_name`)
    /// is looked up in the old dictionary first, a string in the tree first;
    /// the other place is tried too, as readers do.
    pub(crate) fn lookup(&self, doc: &Document, key: &[u8], is_name: bool) -> Option<Vec<Object>> {
        let from_tree = || self.tree_index.get(key).and_then(|&i| self.tree.get(i));
        let from_dict = || self.dict_index.get(key).and_then(|&i| self.dict.get(i));
        let found = if is_name { from_dict().or_else(from_tree) } else { from_tree().or_else(from_dict) };
        explicit(doc, &found?.1)
    }
}

/// The leaves of a name tree (7.9.6) in order: `/Names` arrays of key and
/// value, found through `/Kids`. Cycles and absurd sizes are cut.
fn read_tree(doc: &Document, root: &Object) -> Vec<(Vec<u8>, Object)> {
    let mut out: Vec<(Vec<u8>, Object)> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<(Object, usize)> = vec![(root.clone(), 0)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = stack.pop() {
        if let Object::Ref(r) = &node
            && !seen.insert(r.num)
        {
            continue;
        }
        nodes += 1;
        if nodes > MAX_TREE_NODES {
            break;
        }
        let Ok(Object::Dict(dict)) = doc.resolve(&node) else {
            continue;
        };
        if let Some(Object::Array(items)) = dict.get("Names").and_then(|n| doc.resolve(n).ok()) {
            for [key, value] in items.as_chunks::<2>().0 {
                if out.len() >= MAX_NAMED_DESTS {
                    return out;
                }
                if let Ok(Object::String(key)) = doc.resolve(key) {
                    out.push((key.bytes, value.clone()));
                }
            }
        }
        if depth < MAX_TREE_DEPTH
            && let Some(Object::Array(kids)) = dict.get("Kids").and_then(|k| doc.resolve(k).ok())
        {
            for kid in kids.into_iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    out
}

/// The explicit destination (12.3.2.2) a destination value stands for: an
/// array, or a dictionary whose `/D` is one, followed through references.
pub(crate) fn explicit(doc: &Document, value: &Object) -> Option<Vec<Object>> {
    match doc.resolve(value).ok()? {
        Object::Array(items) => Some(items),
        Object::Dict(d) => match doc.resolve(d.get("D")?).ok()? {
            Object::Array(items) => Some(items),
            _ => None,
        },
        _ => None,
    }
}

/// The page (its object number) an explicit destination array points at, if
/// it points at one of `pages`.
pub(crate) fn array_page(array: &[Object], pages: &HashSet<u32>) -> Option<u32> {
    match array.first()? {
        Object::Ref(r) if pages.contains(&r.num) => Some(r.num),
        _ => None,
    }
}

/// The page a destination value points at: an explicit array, a named
/// destination (looked up in `names`), or a dictionary with a `/D`.
pub(crate) fn dest_page(doc: &Document, value: &Object, names: &NamedDests, pages: &HashSet<u32>) -> Option<u32> {
    let resolved = doc.resolve(value).ok()?;
    match &resolved {
        Object::Name(n) => array_page(&names.lookup(doc, n.as_bytes(), true)?, pages),
        Object::String(s) => array_page(&names.lookup(doc, &s.bytes, false)?, pages),
        other => array_page(&explicit(doc, other)?, pages),
    }
}

/// The page an outline item or link points at, through its `/Dest` or, failing
/// that, a go-to action in `/A` (12.3.2, 12.6.4.2).
pub(crate) fn item_page(doc: &Document, item: &Dict, names: &NamedDests, pages: &HashSet<u32>) -> Option<u32> {
    if let Some(dest) = item.get("Dest") {
        return dest_page(doc, dest, names, pages);
    }
    let Object::Dict(action) = doc.resolve(item.get("A")?).ok()? else {
        return None;
    };
    if !matches!(action.get("S"), Some(Object::Name(n)) if n == "GoTo") {
        return None;
    }
    dest_page(doc, action.get("D")?, names, pages)
}

/// A name tree (7.9.6) for these entries: sorted by key, leaves of at most
/// [`LEAF_SIZE`] entries under as many levels of `/Kids` as it takes. Only the
/// root has no `/Limits`.
pub(crate) fn build_name_tree(mut entries: Vec<(Vec<u8>, Object)>) -> Object {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let limits = |first: &[u8], last: &[u8]| {
        Object::Array(vec![
            Object::String(PdfString::literal(first.to_vec())),
            Object::String(PdfString::literal(last.to_vec())),
        ])
    };
    if entries.len() <= LEAF_SIZE {
        let mut root = Dict::new();
        root.set("Names", Object::Array(flatten_pairs(entries)));
        return Object::Dict(root);
    }
    // Leaves first; then groups of nodes until one is left.
    let mut level: Vec<(Vec<u8>, Vec<u8>, Object)> = Vec::new();
    for chunk in entries.chunks(LEAF_SIZE) {
        let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
            continue;
        };
        let (lo, hi) = (first.0.clone(), last.0.clone());
        let mut leaf = Dict::new();
        leaf.set("Limits", limits(&lo, &hi));
        leaf.set("Names", Object::Array(flatten_pairs(chunk.to_vec())));
        level.push((lo, hi, Object::Dict(leaf)));
    }
    while level.len() > LEAF_SIZE {
        let mut next = Vec::new();
        for group in level.chunks(LEAF_SIZE) {
            let (Some(first), Some(last)) = (group.first(), group.last()) else {
                continue;
            };
            let (lo, hi) = (first.0.clone(), last.1.clone());
            let mut node = Dict::new();
            node.set("Limits", limits(&lo, &hi));
            node.set("Kids", Object::Array(group.iter().map(|(_, _, n)| n.clone()).collect()));
            next.push((lo, hi, Object::Dict(node)));
        }
        level = next;
    }
    let mut root = Dict::new();
    root.set("Kids", Object::Array(level.into_iter().map(|(_, _, n)| n).collect()));
    Object::Dict(root)
}

fn flatten_pairs(entries: Vec<(Vec<u8>, Object)>) -> Vec<Object> {
    let mut out = Vec::with_capacity(entries.len().saturating_mul(2));
    for (key, value) in entries {
        out.push(Object::String(PdfString::literal(key)));
        out.push(value);
    }
    out
}

/// The old-style `/Dests` dictionary for these entries.
pub(crate) fn build_dests_dict(entries: Vec<(Vec<u8>, Object)>) -> Dict {
    Dict::from_pairs(entries.into_iter().map(|(k, v)| (Name::new(k), v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn open(bytes: Vec<u8>) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    #[test]
    fn tree_and_old_dictionary_are_read_and_looked_up() {
        let mut b = PdfBuilder::new();
        b.obj(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 6 0 R >> /Dests << /Old [3 0 R /Fit] /Both [3 0 R /FitH 0] >> >>",
        );
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>");
        b.obj(5, "<< /D [4 0 R /XYZ 0 0 null] >>");
        b.obj(6, "<< /Kids [7 0 R 8 0 R] >>");
        b.obj(7, "<< /Limits [(a) (b)] /Names [(a) [3 0 R /Fit] (b) 5 0 R] >>");
        b.obj(8, "<< /Limits [(Both) (c)] /Names [(Both) [4 0 R /Fit] (c) [4 0 R /Fit]] >>");
        let doc = open(b.finish_classic(9, "/Root 1 0 R"));
        let names = NamedDests::load(&doc);
        assert_eq!(names.tree().len(), 4);
        assert_eq!(names.dict().len(), 2);
        let pages: HashSet<u32> = [3, 4].into_iter().collect();
        // Strings through the tree, also when the value is a dictionary with /D.
        assert_eq!(array_page(&names.lookup(&doc, b"a", false).unwrap(), &pages), Some(3));
        assert_eq!(array_page(&names.lookup(&doc, b"b", false).unwrap(), &pages), Some(4));
        // Names through the old dictionary.
        assert_eq!(array_page(&names.lookup(&doc, b"Old", true).unwrap(), &pages), Some(3));
        // A name falls back to the tree and a string to the dictionary.
        assert_eq!(array_page(&names.lookup(&doc, b"c", true).unwrap(), &pages), Some(4));
        assert_eq!(array_page(&names.lookup(&doc, b"Old", false).unwrap(), &pages), Some(3));
        // The place a kind of key prefers wins.
        assert_eq!(array_page(&names.lookup(&doc, b"Both", true).unwrap(), &pages), Some(3));
        assert_eq!(array_page(&names.lookup(&doc, b"Both", false).unwrap(), &pages), Some(4));
        assert!(names.lookup(&doc, b"nothing", true).is_none());
    }

    #[test]
    fn a_name_tree_with_a_cycle_and_a_missing_catalog_entry_are_harmless() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 3 0 R >> >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, "<< /Kids [3 0 R 4 0 R] >>");
        b.obj(4, "<< /Names [(k) [null /Fit]] /Kids [3 0 R] >>");
        let doc = open(b.finish_classic(5, "/Root 1 0 R"));
        let names = NamedDests::load(&doc);
        assert_eq!(names.tree().len(), 1);
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let doc = open(b.finish_classic(3, "/Root 1 0 R"));
        let names = NamedDests::load(&doc);
        assert!(names.tree().is_empty() && names.dict().is_empty());
    }

    #[test]
    fn a_written_name_tree_is_sorted_and_has_limits_below_the_root() {
        for n in [1usize, 64, 65, 500, 5000] {
            let entries: Vec<(Vec<u8>, Object)> =
                (0..n).rev().map(|i| (format!("d{i:05}").into_bytes(), Object::Integer(i as i64))).collect();
            let tree = build_name_tree(entries);
            let Object::Dict(root) = &tree else { panic!() };
            assert!(!root.contains_key("Limits"));
            // Walk it the way a reader does and count what is in it, in order.
            let mut found: Vec<Vec<u8>> = Vec::new();
            let mut stack = vec![tree.clone()];
            while let Some(node) = stack.pop() {
                let Object::Dict(d) = node else { panic!() };
                if let Some(Object::Array(items)) = d.get("Names") {
                    assert!(items.len() <= 128);
                    for pair in items.chunks(2) {
                        let Object::String(s) = &pair[0] else { panic!() };
                        found.push(s.bytes.clone());
                    }
                }
                if let Some(Object::Array(kids)) = d.get("Kids") {
                    assert!(kids.len() <= 64);
                    for kid in kids.iter().rev() {
                        let Object::Dict(k) = kid else { panic!() };
                        assert!(k.contains_key("Limits"));
                        stack.push(kid.clone());
                    }
                }
            }
            assert_eq!(found.len(), n);
            assert!(found.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
