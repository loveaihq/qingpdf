//! Named destinations (ISO 32000-1 12.3.2.3): the name tree `/Dests` in the
//! catalog's `/Names` dictionary (PDF 1.2) and the old `/Dests` dictionary of
//! the catalog (PDF 1.1), read into flat lists, and the helpers that turn a
//! destination into the page it points at.
//!
//! Everything read here is bounded by bytes ([`Spend`]): a name tree whose
//! leaves share one huge array, or whose keys are one huge string named over
//! and over, ends in [`Error::Limit`] instead of gigabytes of copies.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, Name, ObjRef, Object, PdfString};

/// Most name tree nodes looked at, and most entries kept. Real files have
/// thousands; the limit only stops hostile ones.
const MAX_TREE_NODES: usize = 500_000;
pub(crate) const MAX_NAMED_DESTS: usize = 2_000_000;
/// Deepest name tree followed (7.9.6 trees are a few levels deep).
const MAX_TREE_DEPTH: usize = 64;
/// Bytes of names and destinations that may be copied out of one file while
/// reading and looking up its named destinations. Real files take a few MiB.
const NAMES_BUDGET: usize = 32 * 1024 * 1024;
/// An explicit destination array has at most six elements (Table 151); longer
/// is not a destination.
const MAX_DEST_ARRAY: usize = 16;

/// A name tree leaf holds at most this many entries when a tree is written.
const LEAF_SIZE: usize = 64;

/// What is left of the bytes one document's named destinations may cost.
#[derive(Debug)]
pub(crate) struct Spend(Cell<usize>);

impl Spend {
    pub(crate) fn new(total: usize) -> Spend {
        Spend(Cell::new(total))
    }

    /// Pay for `bytes` of copying; `Limit` when there is not enough left.
    pub(crate) fn take(&self, bytes: usize) -> Result<()> {
        match self.0.get().checked_sub(bytes) {
            Some(left) => {
                self.0.set(left);
                Ok(())
            }
            None => {
                self.0.set(0);
                Err(Error::Limit(format!(
                    "the named destinations ask for more than {} MiB of copying",
                    NAMES_BUDGET / (1024 * 1024)
                )))
            }
        }
    }

    /// `doc.resolve(obj)` for a value of the destinations, paid for by its
    /// size. What cannot be resolved is `None`.
    pub(crate) fn resolved(&self, doc: &Document, obj: &Object) -> Result<Option<Object>> {
        match doc.resolve(obj) {
            Ok(o) => {
                self.take(o.approx_size())?;
                Ok(Some(o))
            }
            Err(_) => Ok(None),
        }
    }
}

/// What the name tree and the old dictionary name, in the order of the file.
/// The values are as they stand there: an array, a dictionary with a `/D`, or
/// a reference to one of those.
#[derive(Debug)]
pub(crate) struct NamedDests {
    tree: Vec<(Vec<u8>, Object)>,
    dict: Vec<(Vec<u8>, Object)>,
    tree_index: HashMap<Vec<u8>, usize>,
    dict_index: HashMap<Vec<u8>, usize>,
    spend: Spend,
}

impl NamedDests {
    /// Read both places from the catalog of `doc`. Whatever cannot be read is
    /// left out; a file that makes this cost more than the budget is a
    /// [`Error::Limit`].
    pub(crate) fn load(doc: &Document) -> Result<NamedDests> {
        let mut out = NamedDests {
            tree: Vec::new(),
            dict: Vec::new(),
            tree_index: HashMap::new(),
            dict_index: HashMap::new(),
            spend: Spend::new(NAMES_BUDGET),
        };
        let Ok(catalog) = doc.catalog() else {
            return Ok(out);
        };
        if let Some(names) = catalog.get("Names").and_then(|n| doc.resolve(n).ok())
            && let Object::Dict(names) = names
            && let Some(root) = names.get("Dests")
        {
            out.tree = read_tree(doc, root, &out.spend)?;
        }
        if let Some(Object::Dict(dests)) = catalog.get("Dests").and_then(|d| doc.resolve(d).ok()) {
            for (key, value) in dests.iter() {
                if out.dict.len() >= MAX_NAMED_DESTS {
                    break;
                }
                out.spend.take(key.as_bytes().len().saturating_add(value.approx_size()))?;
                out.dict.push((key.as_bytes().to_vec(), value.clone()));
            }
        }
        for (i, (key, _)) in out.tree.iter().enumerate() {
            out.tree_index.entry(key.clone()).or_insert(i);
        }
        for (i, (key, _)) in out.dict.iter().enumerate() {
            out.dict_index.entry(key.clone()).or_insert(i);
        }
        Ok(out)
    }

    /// The entries of the name tree, in tree order.
    pub(crate) fn tree(&self) -> &[(Vec<u8>, Object)] {
        &self.tree
    }

    /// The entries of the old `/Dests` dictionary.
    pub(crate) fn dict(&self) -> &[(Vec<u8>, Object)] {
        &self.dict
    }

    /// What everything done with these destinations is paid from.
    pub(crate) fn spend(&self) -> &Spend {
        &self.spend
    }

    /// The explicit destination array called `key`. A name object (`is_name`)
    /// is looked up in the old dictionary first, a string in the tree first;
    /// the other place is tried too, as readers do.
    pub(crate) fn lookup(&self, doc: &Document, key: &[u8], is_name: bool) -> Result<Option<Vec<Object>>> {
        let from_tree = || self.tree_index.get(key).and_then(|&i| self.tree.get(i));
        let from_dict = || self.dict_index.get(key).and_then(|&i| self.dict.get(i));
        let found = if is_name { from_dict().or_else(from_tree) } else { from_tree().or_else(from_dict) };
        match found {
            Some((_, value)) => explicit(doc, value, &self.spend),
            None => Ok(None),
        }
    }
}

/// The leaves of a name tree (7.9.6) in order: `/Names` arrays of key and
/// value, found through `/Kids`. Every node and every array that is an
/// indirect object is read once however many times it is named; cycles and
/// absurd sizes are cut, and what is copied is paid for by its bytes.
fn read_tree(doc: &Document, root: &Object, spend: &Spend) -> Result<Vec<(Vec<u8>, Object)>> {
    let mut out: Vec<(Vec<u8>, Object)> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut once = |obj: &Object| -> bool {
        match obj {
            Object::Ref(r) => seen.insert(r.num),
            _ => true,
        }
    };
    let mut stack: Vec<(Object, usize)> = vec![(root.clone(), 0)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = stack.pop() {
        if !once(&node) {
            continue;
        }
        nodes += 1;
        if nodes > MAX_TREE_NODES {
            break;
        }
        let Some(Object::Dict(dict)) = spend.resolved(doc, &node)? else {
            continue;
        };
        if let Some(names) = dict.get("Names")
            && once(names)
            && let Some(Object::Array(items)) = spend.resolved(doc, names)?
        {
            for [key, value] in items.as_chunks::<2>().0 {
                if out.len() >= MAX_NAMED_DESTS {
                    return Ok(out);
                }
                if let Some(Object::String(key)) = spend.resolved(doc, key)? {
                    spend.take(value.approx_size())?;
                    out.push((key.bytes, value.clone()));
                }
            }
        }
        if depth < MAX_TREE_DEPTH
            && let Some(kids) = dict.get("Kids")
            && once(kids)
            && let Some(Object::Array(kids)) = spend.resolved(doc, kids)?
        {
            for kid in kids.into_iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    Ok(out)
}

/// The explicit destination (12.3.2.2) a destination value stands for: an
/// array, or a dictionary whose `/D` is one, followed through references.
/// An array too long to be a destination is not one.
pub(crate) fn explicit(doc: &Document, value: &Object, spend: &Spend) -> Result<Option<Vec<Object>>> {
    let array = |o: Object| match o {
        Object::Array(items) if items.len() <= MAX_DEST_ARRAY => Some(items),
        _ => None,
    };
    match spend.resolved(doc, value)? {
        Some(Object::Dict(d)) => match d.get("D") {
            Some(inner) => Ok(spend.resolved(doc, inner)?.and_then(array)),
            None => Ok(None),
        },
        Some(other) => Ok(array(other)),
        None => Ok(None),
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
pub(crate) fn dest_page(
    doc: &Document,
    value: &Object,
    names: &NamedDests,
    pages: &HashSet<u32>,
) -> Result<Option<u32>> {
    let Some(resolved) = names.spend().resolved(doc, value)? else {
        return Ok(None);
    };
    let array = match &resolved {
        Object::Name(n) => names.lookup(doc, n.as_bytes(), true)?,
        Object::String(s) => names.lookup(doc, &s.bytes, false)?,
        other => explicit(doc, other, names.spend())?,
    };
    Ok(array.and_then(|a| array_page(&a, pages)))
}

/// The page an outline item or link points at, through its `/Dest` or, failing
/// that, a go-to action in `/A` (12.3.2, 12.6.4.2).
pub(crate) fn item_page(
    doc: &Document,
    item: &Dict,
    names: &NamedDests,
    pages: &HashSet<u32>,
) -> Result<Option<u32>> {
    if let Some(dest) = item.get("Dest") {
        return dest_page(doc, dest, names, pages);
    }
    let Some(action) = item.get("A") else {
        return Ok(None);
    };
    let Some(Object::Dict(action)) = names.spend().resolved(doc, action)? else {
        return Ok(None);
    };
    if !matches!(action.get("S"), Some(Object::Name(n)) if n == "GoTo") {
        return Ok(None);
    }
    match action.get("D") {
        Some(d) => dest_page(doc, d, names, pages),
        None => Ok(None),
    }
}

/// A name tree (7.9.6) for these entries: sorted by key, leaves of at most
/// [`LEAF_SIZE`] entries under as many levels of `/Kids` as it takes. Only the
/// root has no `/Limits`. Every node is an object of its own, given to
/// `define` (which returns the reference to put in the parent): Table 36 says
/// `/Kids` is an array of indirect references, and readers that look nothing
/// up in direct kids (MuPDF) are right to. Returns the root.
pub(crate) fn build_name_tree(
    mut entries: Vec<(Vec<u8>, Object)>,
    define: &mut dyn FnMut(Dict) -> Result<ObjRef>,
) -> Result<ObjRef> {
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
        return define(root);
    }
    // Leaves first; then groups of nodes until one is left.
    let mut level: Vec<(Vec<u8>, Vec<u8>, ObjRef)> = Vec::new();
    for chunk in entries.chunks(LEAF_SIZE) {
        let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
            continue;
        };
        let (lo, hi) = (first.0.clone(), last.0.clone());
        let mut leaf = Dict::new();
        leaf.set("Limits", limits(&lo, &hi));
        leaf.set("Names", Object::Array(flatten_pairs(chunk.to_vec())));
        level.push((lo, hi, define(leaf)?));
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
            node.set("Kids", Object::Array(group.iter().map(|(_, _, r)| Object::Ref(*r)).collect()));
            next.push((lo, hi, define(node)?));
        }
        level = next;
    }
    let mut root = Dict::new();
    root.set("Kids", Object::Array(level.into_iter().map(|(_, _, r)| Object::Ref(r)).collect()));
    define(root)
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
        let names = NamedDests::load(&doc).unwrap();
        assert_eq!(names.tree().len(), 4);
        assert_eq!(names.dict().len(), 2);
        let pages: HashSet<u32> = [3, 4].into_iter().collect();
        let page_of = |key: &[u8], is_name: bool| {
            array_page(&names.lookup(&doc, key, is_name).unwrap().unwrap(), &pages)
        };
        // Strings through the tree, also when the value is a dictionary with /D.
        assert_eq!(page_of(b"a", false), Some(3));
        assert_eq!(page_of(b"b", false), Some(4));
        // Names through the old dictionary.
        assert_eq!(page_of(b"Old", true), Some(3));
        // A name falls back to the tree and a string to the dictionary.
        assert_eq!(page_of(b"c", true), Some(4));
        assert_eq!(page_of(b"Old", false), Some(3));
        // The place a kind of key prefers wins.
        assert_eq!(page_of(b"Both", true), Some(3));
        assert_eq!(page_of(b"Both", false), Some(4));
        assert!(names.lookup(&doc, b"nothing", true).unwrap().is_none());
    }

    #[test]
    fn a_name_tree_with_a_cycle_and_a_missing_catalog_entry_are_harmless() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 3 0 R >> >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, "<< /Kids [3 0 R 4 0 R] >>");
        b.obj(4, "<< /Names [(k) [null /Fit]] /Kids [3 0 R] >>");
        let doc = open(b.finish_classic(5, "/Root 1 0 R"));
        let names = NamedDests::load(&doc).unwrap();
        assert_eq!(names.tree().len(), 1);
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let doc = open(b.finish_classic(3, "/Root 1 0 R"));
        let names = NamedDests::load(&doc).unwrap();
        assert!(names.tree().is_empty() && names.dict().is_empty());
    }

    #[test]
    fn leaves_that_share_one_huge_names_array_are_read_once_and_paid_for() {
        // 1500 leaves all name the same indirect /Names array (a 1 MiB key):
        // read once it is nothing; read per leaf it would be 1.5 GB.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 10 0 R >> >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.raw_obj(8, &[b"8 0 obj\n(".as_slice(), &vec![b'K'; 1 << 20], b")\nendobj\n"].concat());
        b.obj(9, "[8 0 R [4 0 R /Fit]]");
        let kids: String = (0..1500).map(|i| format!("{} 0 R ", 100 + i)).collect();
        b.obj(10, &format!("<< /Kids [{kids}] >>"));
        for i in 0..1500 {
            b.obj(100 + i, "<< /Limits [(a) (z)] /Names 9 0 R >>");
        }
        let doc = open(b.finish_classic(1600, "/Root 1 0 R"));
        let started = std::time::Instant::now();
        let names = NamedDests::load(&doc).unwrap();
        assert_eq!(names.tree().len(), 1);
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }

    #[test]
    fn one_huge_key_named_over_and_over_is_a_limit_error() {
        // The same 1 MiB indirect string as the key of 1500 pairs.
        let pairs = "8 0 R [4 0 R /Fit] ".repeat(1500);
        let mut b = PdfBuilder::new();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [{pairs}] >> >> >>"));
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.raw_obj(8, &[b"8 0 obj\n(".as_slice(), &vec![b'K'; 1 << 20], b")\nendobj\n"].concat());
        let doc = open(b.finish_classic(9, "/Root 1 0 R"));
        let started = std::time::Instant::now();
        assert!(matches!(NamedDests::load(&doc), Err(Error::Limit(_))));
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }

    #[test]
    fn an_array_too_long_to_be_a_destination_is_not_one() {
        let long = "0 ".repeat(100);
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, &format!("[{long}]"));
        b.obj(4, "[5 0 R /Fit]");
        let doc = open(b.finish_classic(5, "/Root 1 0 R"));
        let spend = Spend::new(1 << 20);
        assert!(explicit(&doc, &Object::Ref(ObjRef::new(3, 0)), &spend).unwrap().is_none());
        assert_eq!(explicit(&doc, &Object::Ref(ObjRef::new(4, 0)), &spend).unwrap().unwrap().len(), 2);
    }

    /// Nodes handed to `define` by a tree under construction, by number.
    fn build(entries: Vec<(Vec<u8>, Object)>) -> (ObjRef, HashMap<u32, Dict>) {
        let mut nodes: HashMap<u32, Dict> = HashMap::new();
        let mut next = 1000u32;
        let root = build_name_tree(entries, &mut |d| {
            next += 1;
            nodes.insert(next, d);
            Ok(ObjRef::new(next, 0))
        })
        .unwrap();
        (root, nodes)
    }

    #[test]
    fn a_written_name_tree_is_sorted_has_limits_below_the_root_and_only_indirect_kids() {
        for n in [1usize, 64, 65, 500, 5000, 70_000] {
            let entries: Vec<(Vec<u8>, Object)> =
                (0..n).rev().map(|i| (format!("d{i:05}").into_bytes(), Object::Integer(i as i64))).collect();
            let (root, nodes) = build(entries);
            assert!(!nodes[&root.num].contains_key("Limits"));
            // Walk it the way a reader does and count what is in it, in order.
            let mut found: Vec<Vec<u8>> = Vec::new();
            let mut stack = vec![root];
            while let Some(at) = stack.pop() {
                let d = &nodes[&at.num];
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
                        // Table 36: an array of indirect references.
                        let Object::Ref(k) = kid else { panic!("a direct kid: {kid:?}") };
                        assert!(nodes[&k.num].contains_key("Limits"));
                        stack.push(*k);
                    }
                }
            }
            assert_eq!(found.len(), n);
            assert!(found.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
