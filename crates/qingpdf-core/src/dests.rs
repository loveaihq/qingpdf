//! Named destinations (ISO 32000-1 12.3.2.3): the name tree `/Dests` in the
//! catalog's `/Names` dictionary (PDF 1.2) and the old `/Dests` dictionary of
//! the catalog (PDF 1.1), read into flat lists, and the helpers that turn a
//! destination into the page it points at.
//!
//! Everything read here is bounded by bytes ([`Spend`]): a name tree whose
//! leaves share one huge array, or whose keys are one huge string named over
//! and over, ends in [`Error::Limit`] instead of gigabytes of copies. What the
//! budget stops is amplification (the same big thing copied again and again),
//! not work in proportion to the file: it grows with the size of the file.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

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
/// reading and looking up its named destinations: at least this much ...
const NAMES_BUDGET_MIN: usize = 32 * 1024 * 1024;
/// ... and this many times the size of the file, because a file with two
/// hundred thousand named destinations (a book made with hyperref) is 18 MB
/// and needs to hold all of them.
const NAMES_BUDGET_PER_FILE_BYTE: usize = 4;
/// A copy of this many bytes or fewer is not paid for when it is made again:
/// a destination is about a hundred bytes, and looking one up three hundred
/// thousand times is work in proportion to the file. Bigger copies pay every
/// time, which is what stops one big thing being copied over and over.
const FREE_COPY: usize = 512;

/// How many bytes the named destinations of a file of `file_size` bytes may cost.
fn names_budget(file_size: usize) -> usize {
    NAMES_BUDGET_MIN.max(file_size.saturating_mul(NAMES_BUDGET_PER_FILE_BYTE))
}
/// An explicit destination array has at most six elements (Table 151); longer
/// is not a destination.
const MAX_DEST_ARRAY: usize = 16;

/// A name tree leaf holds at most this many entries when a tree is written.
const LEAF_SIZE: usize = 64;

/// What a node of a name tree counts for beside its entries, in [`NamedDests::load_with`]'s steps.
const NODE_STEPS: usize = 16;

/// What an entry of the lists costs besides its key and value: the pair and the
/// key's vector.
const ENTRY_OVERHEAD: usize = 48;

/// What is left of the bytes one document's named destinations may cost.
#[derive(Debug)]
pub(crate) struct Spend {
    left: Cell<usize>,
    total: usize,
    /// The indirect objects that have been resolved, with their sizes: the first
    /// time one is paid for in full, as a distinct object; again only if it is
    /// bigger than [`FREE_COPY`].
    seen: RefCell<HashMap<u32, usize>>,
}

impl Spend {
    pub(crate) fn new(total: usize) -> Spend {
        Spend { left: Cell::new(total), total, seen: RefCell::new(HashMap::new()) }
    }

    /// Pay for `bytes` of copying; `Limit` when there is not enough left.
    pub(crate) fn take(&self, bytes: usize) -> Result<()> {
        match self.left.get().checked_sub(bytes) {
            Some(left) => {
                self.left.set(left);
                Ok(())
            }
            None => {
                self.left.set(0);
                Err(Error::Limit(format!(
                    "the named destinations ask for more than {} MiB of copying",
                    self.total / (1024 * 1024)
                )))
            }
        }
    }

    /// Pay for a copy that is thrown away again: free up to [`FREE_COPY`] bytes.
    fn take_copy(&self, bytes: usize) -> Result<()> {
        self.take(bytes.saturating_sub(FREE_COPY))
    }

    /// `doc.resolve(obj)` for a value of the destinations. A distinct indirect
    /// object is paid for once, by its size; a repeated one, and a direct one
    /// (which is copied each time), only if it is more than [`FREE_COPY`] bytes.
    /// What cannot be resolved is `None`. A repeated reference whose copy the
    /// budget can no longer pay for is not read at all (3d-2 review: twenty
    /// thousand items naming one megabyte were each parsed before being refused).
    pub(crate) fn resolved(&self, doc: &Document, obj: &Object) -> Result<Option<Object>> {
        if let Object::Ref(r) = obj {
            let known = self.seen.borrow().get(&r.num).copied();
            if let Some(size) = known {
                self.take_copy(size)?;
            }
        }
        let Ok(o) = doc.resolve(obj) else {
            return Ok(None);
        };
        let size = o.approx_size();
        match obj {
            Object::Ref(r) => {
                let first = self.seen.borrow_mut().insert(r.num, size).is_none();
                if first {
                    self.take(size)?;
                }
            }
            _ => self.take_copy(size)?,
        }
        Ok(Some(o))
    }
}

/// An entry looked up: the explicit destination (`None` when the value is not
/// one) and the bytes it takes.
type LookedUp = (Option<Rc<Vec<Object>>>, usize);

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
    /// The explicit destinations of the entries already looked up (the tree's
    /// or the dictionary's, by position), and their sizes: a destination is
    /// resolved once however many links name it.
    looked_up: RefCell<HashMap<(bool, usize), LookedUp>>,
}

impl NamedDests {
    /// Read both places from the catalog of `doc`. Whatever cannot be read is
    /// left out; a file that makes this cost more than the budget is a
    /// [`Error::Limit`].
    pub(crate) fn load(doc: &Document) -> Result<NamedDests> {
        NamedDests::load_with(doc, &|_| true)
    }

    /// [`NamedDests::load`] that asks `charge` for the steps it is about to take (an entry is one step, a node of the tree
    /// [`NODE_STEPS`]) and stops with [`Error::Cancelled`] when it says no, so that the work can be metered and cancelled.
    pub(crate) fn load_with(doc: &Document, charge: &dyn Fn(usize) -> bool) -> Result<NamedDests> {
        let mut out = NamedDests {
            tree: Vec::new(),
            dict: Vec::new(),
            tree_index: HashMap::new(),
            dict_index: HashMap::new(),
            spend: Spend::new(names_budget(doc.file_size())),
            looked_up: RefCell::new(HashMap::new()),
        };
        let Ok(catalog) = doc.catalog() else {
            return Ok(out);
        };
        if let Some(names) = catalog.get("Names").and_then(|n| doc.resolve(n).ok())
            && let Object::Dict(names) = names
            && let Some(root) = names.get("Dests")
        {
            out.tree = read_tree(doc, root, &out.spend, charge)?;
        }
        if let Some(Object::Dict(dests)) = catalog.get("Dests").and_then(|d| doc.resolve(d).ok()) {
            if !charge(dests.len().min(MAX_NAMED_DESTS)) {
                return Err(Error::Cancelled);
            }
            for (key, value) in dests.iter() {
                if out.dict.len() >= MAX_NAMED_DESTS {
                    break;
                }
                out.spend.take(key.as_bytes().len().saturating_add(value.approx_size()).saturating_add(ENTRY_OVERHEAD))?;
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
        let from_tree = || self.tree_index.get(key).and_then(|&i| self.tree.get(i).map(|e| ((true, i), e)));
        let from_dict = || self.dict_index.get(key).and_then(|&i| self.dict.get(i).map(|e| ((false, i), e)));
        let found = if is_name { from_dict().or_else(from_tree) } else { from_tree().or_else(from_dict) };
        let Some((at, (_, value))) = found else {
            return Ok(None);
        };
        let cached = self.looked_up.borrow().get(&at).cloned();
        let (array, size) = match cached {
            Some(known) => known,
            None => {
                let array = explicit(doc, value, &self.spend)?;
                let size = array.as_ref().map_or(0, |a| a.iter().map(Object::approx_size).sum::<usize>());
                let entry = (array.map(Rc::new), size);
                self.looked_up.borrow_mut().insert(at, entry.clone());
                entry
            }
        };
        // The copy handed out is paid for if it is big.
        self.spend.take_copy(size)?;
        Ok(array.map(|a| a.as_ref().clone()))
    }
}

/// The leaves of a name tree (7.9.6) in order: `/Names` arrays of key and
/// value, found through `/Kids`. Every node and every array that is an
/// indirect object is read once however many times it is named; cycles and
/// absurd sizes are cut, and what is copied is paid for by its bytes.
fn read_tree(doc: &Document, root: &Object, spend: &Spend, charge: &dyn Fn(usize) -> bool) -> Result<Vec<(Vec<u8>, Object)>> {
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
        if !charge(NODE_STEPS) {
            return Err(Error::Cancelled);
        }
        // Each distinct node and array is read once (above), so reading them
        // is work in proportion to the file; only what is kept is paid for.
        let Ok(Object::Dict(dict)) = doc.resolve(&node) else {
            continue;
        };
        if let Some(names) = dict.get("Names")
            && once(names)
            && let Ok(Object::Array(items)) = doc.resolve(names)
        {
            if !charge(items.len() / 2) {
                return Err(Error::Cancelled);
            }
            for [key, value] in items.as_chunks::<2>().0 {
                if out.len() >= MAX_NAMED_DESTS {
                    return Ok(out);
                }
                if let Some(Object::String(key)) = spend.resolved(doc, key)? {
                    spend.take(key.bytes.len().saturating_add(value.approx_size()).saturating_add(ENTRY_OVERHEAD))?;
                    out.push((key.bytes, value.clone()));
                }
            }
        }
        if depth < MAX_TREE_DEPTH
            && let Some(kids) = dict.get("Kids")
            && once(kids)
            && let Ok(Object::Array(kids)) = doc.resolve(kids)
        {
            if !charge(kids.len()) {
                return Err(Error::Cancelled);
            }
            for kid in kids.into_iter().rev() {
                spend.take(kid.approx_size())?;
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

/// The explicit destination array a destination value stands for: the array itself, a named destination (looked up in
/// `names`), or a dictionary with a `/D`.
pub(crate) fn dest_array(doc: &Document, value: &Object, names: &NamedDests) -> Result<Option<Vec<Object>>> {
    let Some(resolved) = names.spend().resolved(doc, value)? else {
        return Ok(None);
    };
    match &resolved {
        Object::Name(n) => names.lookup(doc, n.as_bytes(), true),
        Object::String(s) => names.lookup(doc, &s.bytes, false),
        other => explicit(doc, other, names.spend()),
    }
}

/// The explicit destination an outline item or link points at, through its `/Dest` or, failing that, a go-to action in
/// `/A` (12.3.2, 12.6.4.2).
pub(crate) fn item_array(doc: &Document, item: &Dict, names: &NamedDests) -> Result<Option<Vec<Object>>> {
    if let Some(dest) = item.get("Dest") {
        return dest_array(doc, dest, names);
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
        Some(d) => dest_array(doc, d, names),
        None => Ok(None),
    }
}

/// The page an outline item or link points at, through its `/Dest` or, failing that, a go-to action in `/A` (12.3.2,
/// 12.6.4.2).
pub(crate) fn item_page(
    doc: &Document,
    item: &Dict,
    names: &NamedDests,
    pages: &HashSet<u32>,
) -> Result<Option<u32>> {
    Ok(item_array(doc, item, names)?.and_then(|a| array_page(&a, pages)))
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
    fn the_budget_grows_with_the_size_of_the_file() {
        assert_eq!(names_budget(0), 32 * 1024 * 1024);
        assert_eq!(names_budget(1 << 20), 32 * 1024 * 1024);
        assert_eq!(names_budget(18 * 1024 * 1024), 72 * 1024 * 1024);
        assert_eq!(names_budget(usize::MAX), usize::MAX);
    }

    /// A book made with hyperref: `n` named destinations, each a small array that
    /// is an object of its own, in leaves of 64 under one root.
    fn many_small_destinations(n: u32) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        let first_dest = 20u32;
        let leaves = n.div_ceil(64);
        let first_leaf = first_dest + n;
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 3 0 R >> >>");
        b.obj(2, "<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 9 9] >>");
        let kids: String = (0..leaves).map(|i| format!("{} 0 R ", first_leaf + i)).collect();
        b.obj(3, &format!("<< /Kids [{kids}] >>"));
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        for i in 0..n {
            b.obj(first_dest + i, "[4 0 R /XYZ 72 700 null]");
        }
        for leaf in 0..leaves {
            let (from, to) = (leaf * 64, ((leaf + 1) * 64).min(n));
            let pairs: String = (from..to).map(|i| format!("(eq.{i:07}) {} 0 R ", first_dest + i)).collect();
            b.obj(
                first_leaf + leaf,
                &format!("<< /Limits [(eq.{from:07}) (eq.{:07})] /Names [{pairs}] >>", to - 1),
            );
        }
        b.finish_classic(first_leaf + leaves, "/Root 1 0 R")
    }

    #[test]
    fn two_hundred_thousand_small_named_destinations_are_within_the_budget() {
        // 200,000 of them were refused at 32 MiB; they are a file of 18 MB.
        let n = 200_000u32;
        let doc = open(many_small_destinations(n));
        let names = NamedDests::load(&doc).unwrap();
        assert_eq!(names.tree().len(), n as usize);
        let pages: HashSet<u32> = [4].into_iter().collect();
        // Every one of them can be resolved for the page it is on (as `Index::build` does).
        for (_, value) in names.tree() {
            let array = explicit(&doc, value, names.spend()).unwrap().unwrap();
            assert_eq!(array_page(&array, &pages), Some(4));
        }
    }

    #[test]
    fn looking_up_the_same_small_destination_again_and_again_costs_nothing() {
        // 400,000 links to one destination (of about 100 bytes) used to cost the
        // whole 32 MiB; it is looked up once and copied, which is free when small.
        let doc = open(many_small_destinations(100));
        let names = NamedDests::load(&doc).unwrap();
        for _ in 0..400_000 {
            let array = names.lookup(&doc, b"eq.0000007", false).unwrap().unwrap();
            assert_eq!(array.len(), 5);
        }
        assert!(names.lookup(&doc, b"nothing", false).unwrap().is_none());
    }

    #[test]
    fn a_big_destination_looked_up_over_and_over_pays_every_time() {
        // A destination with a 100,000-byte string in it: free for nobody.
        let junk = "A".repeat(100_000);
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [(big) 5 0 R] >> >> >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(5, &format!("[3 0 R /XYZ ({junk})]"));
        let doc = open(b.finish_classic(6, "/Root 1 0 R"));
        let names = NamedDests::load(&doc).unwrap();
        let mut refused_at = None;
        for i in 0..2000 {
            match names.lookup(&doc, b"big", false) {
                Ok(Some(_)) => {}
                Err(Error::Limit(_)) => {
                    refused_at = Some(i);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        // 32 MiB of 100 KB copies: about 335 of them.
        assert!(refused_at.is_some_and(|i| (300..400).contains(&i)), "{refused_at:?}");
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
