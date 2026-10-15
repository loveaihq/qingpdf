//! Pages turned, taken out, moved, and put in from another file (the list of thumbnails, 2026-10-15).
//!
//! A change to the pages is a change to a few objects, like every edit: the nodes of the page tree whose `/Kids` or `/Count` change,
//! the dictionaries of the pages turned (`/Rotate`) or moved to another node (`/Parent`), the form (`/AcroForm`) when a page with
//! fields goes or comes, and the objects of the pages that come in. They are put in front of the document's own objects (`commit`),
//! undone and redone with the other edits and written as the one incremental update when the file is saved; the pages themselves
//! (their content, their annotations, the edits made to them) are not touched, so they stay with their page wherever it goes.
//!
//! The page tree keeps its shape (7.7.3.2): turning changes no node; a move puts the pages into the places the tree has for pages
//! (the place of the first page in the tree gets the page that is to be first, and so on), so the nodes keep the same number of pages
//! and a flat tree stays flat; taking pages out takes them from the `/Kids` of the nodes that hold them (a node left with no page goes
//! too) and puts the counts right all the way up; pages put in go next to the page they come before or after. A page that moves to
//! another node takes what it inherited along (7.7.3.4). Other readers and `qpdf --check` see an ordinary tree.
//!
//! What is not touched: bookmarks and links that lead to a page that has gone (they lead nowhere; other readers ignore them), the
//! old pages in the earlier part of a saved file (a whole rewrite, "save as", leaves them out).

use std::collections::{HashMap, HashSet};
use std::fmt;

use super::{Ctx, TOO_BIG};
use crate::document::{self, Document, NodeKind, Page, classify};
use crate::error::{Error, Result};
use crate::object::{Dict, Name, ObjRef, Object};
use crate::ops;
use crate::render::work::Work;
use crate::writer::Builder;

/// Most pages one change puts in, and most a document may have afterwards (a change that would leave more is refused).
pub const MAX_INSERT_PAGES: usize = 5_000;
pub const MAX_PAGES_AFTER: usize = 100_000;
/// The biggest file pages are put in from: what the memory set aside for what is not saved yet holds (the objects of the pages that come
/// in are about as big as the file they come from, so a bigger file could not be put in anyway).
pub const MAX_INSERT_FILE_BYTES: u64 = super::DEFAULT_ROOM as u64;
/// Deepest page tree and most entries in it a change looks at (the reader's own limits are as high).
const MAX_TREE_DEPTH: usize = 256;
const MAX_TREE_VISITS: usize = 2_000_000;
/// What a change costs the work meter (about a nanosecond a unit): the change, each page of the tree it reads, each byte put in.
const OP_WORK: f64 = 5.0e6;
const PAGE_UNIT_WORK: f64 = 2.0e4;
const IMPORT_BYTE_WORK: f64 = 8.0;
/// What a field of the other file's form (or an entry of its resources) costs: looking at it and what is done with its name.
const FIELD_WORK: f64 = 2.0e3;
/// Most fields a form that is put in may have (the number the signature check of a file gives up at), and most entries its resources may
/// have for them to be merged.
const MAX_FORM_FIELDS: usize = 20_000;
const MAX_DR_ENTRIES: usize = 10_000;
/// The kinds of resources a form's `/DR` is merged for (the names of 7.8.3 that hold dictionaries of named resources).
const RESOURCE_KINDS: [&str; 6] = ["Font", "XObject", "ExtGState", "ColorSpace", "Pattern", "Shading"];
/// The inheritable entries of a page (7.7.3.4).
const INHERITED: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// A password that is not shown in a log and is overwritten when it is let go of.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Password(String);

impl Password {
    pub fn new(password: &str) -> Password {
        Password(password.to_string())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(..)")
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

/// One change to the pages of a document. Pages are counted from 0.
#[derive(Clone, Debug, PartialEq)]
pub enum PageOp {
    /// Turn the pages (in any order, each once) clockwise by `degrees`, a multiple of 90 (a negative number turns them the other way).
    Rotate { pages: Vec<u32>, degrees: i32 },
    /// Take the pages out; not all of them.
    Delete { pages: Vec<u32> },
    /// Put the pages, kept in their order, in front of page `to` as the pages are before the change (the number of pages is the end).
    Move { pages: Vec<u32>, to: u32 },
    /// Put all the pages of the file at `path`, opened with `password`, in front of page `at` (the number of pages is the end).
    Insert { at: u32, path: String, password: Password },
}

impl PageOp {
    /// How many pages the document has after the change, if that is known without doing it (`None` for a file to put in).
    pub fn count_after(&self, before: usize) -> Option<usize> {
        match self {
            PageOp::Rotate { .. } | PageOp::Move { .. } => Some(before),
            PageOp::Delete { pages } => Some(before.saturating_sub(unique(pages, before).len())),
            PageOp::Insert { .. } => None,
        }
    }
}

/// The pages of a list that are among the first `count`, ascending and each once.
pub(crate) fn unique(which: &[u32], count: usize) -> Vec<u32> {
    let mut v: Vec<u64> = which.iter().filter(|&&p| (p as usize) < count).map(|&p| u64::from(p)).collect();
    crate::order::sort_u64(&mut v);
    v.dedup();
    v.into_iter().map(|p| p as u32).collect()
}

/// The order of `count` pages (each as the number it had) after the pages in `pages` are put, kept in their order, in front of page
/// `to`: what the reader shows after a move, and what the engine does.
pub fn order_after_move(count: usize, pages: &[u32], to: usize) -> Vec<u32> {
    let moved = unique(pages, count);
    let stays = |p: &u32| moved.binary_search(p).is_err();
    let rest: Vec<u32> = (0..u32::try_from(count).unwrap_or(u32::MAX)).filter(stays).collect();
    // The pages that stay and come before `to` are the ones the moved pages go after.
    let before = (0..u32::try_from(to.min(count)).unwrap_or(u32::MAX)).filter(stays).count();
    let (head, tail) = rest.split_at(before.min(rest.len()));
    head.iter().chain(&moved).chain(tail).copied().collect()
}

fn odd(what: &str) -> Error {
    Error::Invalid(what.to_string())
}

/// Object numbers in order, to look in.
fn sorted_nums(nums: impl Iterator<Item = u32>) -> Vec<u64> {
    let mut v: Vec<u64> = nums.map(u64::from).collect();
    crate::order::sort_u64(&mut v);
    v
}

fn has(sorted: &[u64], num: u32) -> bool {
    sorted.binary_search(&u64::from(num)).is_ok()
}

// --- the page tree -----------------------------------------------------------------------------------------------------------

/// A page-tree node (7.7.3.2) as it was read.
struct Node {
    obj: ObjRef,
    dict: Dict,
    parent: Option<usize>,
    kids: Vec<Object>,
}

/// A page: the node that lists it and its place in that node's `/Kids`.
struct Slot {
    page: ObjRef,
    node: usize,
    at: usize,
}

/// The page tree: its nodes, the root first and parents before their kids, and its pages in order.
struct Tree {
    nodes: Vec<Node>,
    slots: Vec<Slot>,
}

fn kids_of(doc: &Document, d: &Dict) -> Result<Vec<Object>> {
    match d.get("Kids").map(|k| doc.resolve(k)) {
        Some(Ok(Object::Array(a))) => Ok(a),
        Some(Err(e)) => Err(e),
        _ => Ok(Vec::new()),
    }
}

impl Tree {
    /// Read the tree the way the reader does (`Document::pages`), but one that lists a page or a node twice is refused: a change to a page
    /// that is in two places would be a change to both.
    fn read(doc: &Document) -> Result<Tree> {
        let root = match doc.catalog()?.get("Pages") {
            Some(Object::Ref(r)) => *r,
            _ => return Err(odd("the file has no page tree")),
        };
        let Object::Dict(root_dict) = doc.get(root)? else { return Err(odd("the root of the page tree is not a dictionary")) };
        if !matches!(classify(&root_dict), NodeKind::Pages) {
            return Err(odd("the root of the page tree is not a page-tree node"));
        }
        let kids = kids_of(doc, &root_dict)?;
        let mut tree = Tree { nodes: vec![Node { obj: root, dict: root_dict, parent: None, kids }], slots: Vec::new() };
        let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
        let mut visits = 0usize;
        while let Some(top) = stack.last_mut() {
            let (ni, next) = *top;
            top.1 += 1;
            visits += 1;
            if visits > MAX_TREE_VISITS {
                return Err(Error::Limit(format!("more than {MAX_TREE_VISITS} entries in the page tree")));
            }
            let Some(kid) = tree.nodes.get(ni).and_then(|n| n.kids.get(next)).cloned() else {
                stack.pop();
                continue;
            };
            let Object::Ref(r) = kid else { continue };
            let (dict, is_stream) = match doc.get(r)? {
                Object::Dict(d) => (d, false),
                Object::Stream(s) => (s.dict, true),
                _ => continue,
            };
            match classify(&dict) {
                NodeKind::Skip => {}
                _ if is_stream => return Err(odd("a page or a page-tree node is a stream")),
                NodeKind::Pages => {
                    if stack.len() >= MAX_TREE_DEPTH {
                        return Err(Error::Limit(format!("page tree nested more than {MAX_TREE_DEPTH} levels deep")));
                    }
                    let kids = kids_of(doc, &dict)?;
                    tree.nodes.push(Node { obj: r, dict, parent: Some(ni), kids });
                    stack.push((tree.nodes.len() - 1, 0));
                }
                NodeKind::Page => {
                    if tree.slots.len() >= document::MAX_PAGES {
                        return Err(Error::Limit(format!("more than {} pages in the page tree", document::MAX_PAGES)));
                    }
                    tree.slots.push(Slot { page: r, node: ni, at: next });
                }
            }
        }
        // A page or a node listed twice (a cycle ends above, at the depth limit): a change to it would be a change to both places.
        let twice = |nums: Vec<u64>| nums.windows(2).any(|w| w.first() == w.get(1));
        if twice(sorted_nums(tree.slots.iter().map(|s| s.page.num))) || twice(sorted_nums(tree.nodes.iter().map(|n| n.obj.num))) {
            return Err(odd("the page tree lists a page or a node twice"));
        }
        Ok(tree)
    }

    /// The value of an inheritable entry in force at `node` (its own or the nearest ancestor's) and the node it comes from.
    fn inherited(&self, mut node: usize, key: &str) -> Option<(usize, &Object)> {
        for _ in 0..=MAX_TREE_DEPTH {
            let n = self.nodes.get(node)?;
            if let Some(v) = n.dict.get(key) {
                return Some((node, v));
            }
            node = n.parent?;
        }
        None
    }

    /// The pages under each node, from the kids that are among `pages` (object numbers, in order) and the nodes below.
    fn leaves(&self, pages: &[u64]) -> Vec<usize> {
        let mut leaves: Vec<usize> = self.nodes.iter().map(|n| n.kids.iter().filter(|k| matches!(k, Object::Ref(r) if has(pages, r.num))).count()).collect();
        for i in (1..self.nodes.len()).rev() {
            let (own, parent) = (leaves.get(i).copied().unwrap_or(0), self.nodes.get(i).and_then(|n| n.parent));
            if let Some(p) = parent.and_then(|p| leaves.get_mut(p)) {
                *p += own;
            }
        }
        leaves
    }
}

/// Write the nodes marked in `dirty`, with the counts `leaves` says.
fn write_nodes(objects: &mut Vec<(u32, Object)>, tree: &Tree, dirty: &[bool], leaves: &[usize]) {
    for (i, node) in tree.nodes.iter().enumerate().filter(|(i, _)| dirty.get(*i).copied().unwrap_or(false)) {
        let mut d = node.dict.clone();
        d.set("Kids", Object::Array(node.kids.clone()));
        d.set("Count", Object::Integer(i64::try_from(leaves.get(i).copied().unwrap_or(0)).unwrap_or(0)));
        objects.push((node.obj.num, Object::Dict(d)));
    }
}

/// The dictionary of a page, as the document has it now.
fn dict_of(doc: &Document, r: ObjRef) -> Result<Dict> {
    match doc.get(r)? {
        Object::Dict(d) => Ok(d),
        _ => Err(odd("a page is not a dictionary")),
    }
}

// --- the changes ---------------------------------------------------------------------------------------------------------------

/// The document catalog while a change is worked out: written back only if the change touched it.
struct Catalog {
    obj: ObjRef,
    dict: Dict,
    changed: bool,
}

impl Catalog {
    fn read(doc: &Document) -> Result<Catalog> {
        let obj = match doc.trailer().get("Root") {
            Some(Object::Ref(r)) => *r,
            _ => return Err(odd("the file has no catalog")),
        };
        Ok(Catalog { obj, dict: doc.catalog()?, changed: false })
    }

    fn remove(&mut self, key: &str) -> bool {
        let had = self.dict.remove(key).is_some();
        self.changed |= had;
        had
    }

    fn set(&mut self, key: &str, value: Object) {
        self.dict.set(key, value);
        self.changed = true;
    }
}

/// Do `op` to the document of `ctx`, whose pages are `pages`: the objects it makes or changes go to `ctx.objects`.
pub(super) fn apply(ctx: &mut Ctx<'_>, pages: &[Page], op: &PageOp) -> Result<()> {
    let tree = Tree::read(ctx.doc)?;
    if tree.slots.len() != pages.len() || tree.slots.iter().zip(pages).any(|(s, p)| s.page.num != p.obj_ref.num) {
        return Err(odd("the page tree is not as it was read: reload the file"));
    }
    if !ctx.work.charge(OP_WORK + PAGE_UNIT_WORK * pages.len() as f64) {
        return Err(Error::Limit("too many changes to the pages in this session: save the file and open it again".to_string()));
    }
    let from = ctx.objects.len();
    let mut catalog = Catalog::read(ctx.doc)?;
    match op {
        PageOp::Rotate { pages: which, degrees } => rotate(ctx, &tree, pages, which, *degrees)?,
        PageOp::Delete { pages: which } => delete(ctx, tree, pages, which, &mut catalog)?,
        PageOp::Move { pages: which, to } => move_pages(ctx, tree, pages, which, *to as usize, &mut catalog)?,
        PageOp::Insert { at, path, password } => insert(ctx, tree, pages, *at as usize, path, password, &mut catalog)?,
    }
    if catalog.changed {
        ctx.objects.push((catalog.obj.num, Object::Dict(catalog.dict)));
    }
    ctx.tally(from)
}

/// The unique pages of a list, ascending; one that is not a page of the document is an error.
fn pick(which: &[u32], count: usize) -> Result<Vec<u32>> {
    let picked = unique(which, count);
    if which.iter().any(|&p| p as usize >= count) {
        return Err(odd("there is no such page"));
    }
    Ok(picked)
}

fn rotate(ctx: &mut Ctx<'_>, tree: &Tree, pages: &[Page], which: &[u32], degrees: i32) -> Result<()> {
    if degrees % 90 != 0 {
        return Err(odd("a page is turned by a multiple of 90 degrees"));
    }
    let which = pick(which, pages.len())?;
    if which.is_empty() || degrees.rem_euclid(360) == 0 {
        return Err(odd("there is nothing to turn"));
    }
    for p in which {
        let (Some(page), Some(slot)) = (pages.get(p as usize), tree.slots.get(p as usize)) else { continue };
        let turned = (page.rotate() + i64::from(degrees)).rem_euclid(360);
        let mut dict = dict_of(ctx.doc, slot.page)?;
        // What the page would have without a /Rotate of its own: if that is the new turn, it needs none.
        let inherited = tree.inherited(slot.node, "Rotate").map_or(0, |(_, v)| match ctx.doc.resolve(v) {
            Ok(Object::Integer(i)) => i.rem_euclid(360) / 90 * 90,
            Ok(Object::Real(r)) if r.is_finite() => (r.round() as i64).rem_euclid(360) / 90 * 90,
            _ => 0,
        });
        if turned == inherited {
            dict.remove("Rotate");
        } else {
            dict.set("Rotate", Object::Integer(turned));
        }
        ctx.objects.push((slot.page.num, Object::Dict(dict)));
    }
    Ok(())
}

fn delete(ctx: &mut Ctx<'_>, mut tree: Tree, pages: &[Page], which: &[u32], catalog: &mut Catalog) -> Result<()> {
    let which = pick(which, pages.len())?;
    if which.is_empty() {
        return Err(odd("no pages are chosen"));
    }
    if which.len() >= pages.len() {
        return Err(odd("a document needs at least one page"));
    }
    let gone: Vec<u32> = which.iter().filter_map(|&p| pages.get(p as usize)).map(|p| p.obj_ref.num).collect();
    let gone_sorted = sorted_nums(gone.iter().copied());
    let all = sorted_nums(pages.iter().map(|p| p.obj_ref.num));
    let before = tree.leaves(&all);
    let mut dirty = vec![false; tree.nodes.len()];
    for (node, flag) in tree.nodes.iter_mut().zip(&mut dirty) {
        let n = node.kids.len();
        node.kids.retain(|k| !matches!(k, Object::Ref(r) if has(&gone_sorted, r.num)));
        *flag = node.kids.len() != n;
    }
    let kept: Vec<u64> = all.iter().copied().filter(|n| gone_sorted.binary_search(n).is_err()).collect();
    let leaves = tree.leaves(&kept);
    // A node with no page left is taken out of its parent (the root stays).
    for i in (1..tree.nodes.len()).rev() {
        if leaves.get(i).copied().unwrap_or(0) != 0 {
            continue;
        }
        let Some((obj, parent)) = tree.nodes.get(i).map(|n| (n.obj, n.parent)) else { continue };
        if let Some(p) = parent.and_then(|p| tree.nodes.get_mut(p)) {
            p.kids.retain(|k| !matches!(k, Object::Ref(r) if r.num == obj.num));
        }
        if let Some(flag) = parent.and_then(|p| dirty.get_mut(p)) {
            *flag = true;
        }
    }
    // The counts above a change are put right even where the kids stay.
    for (i, (a, b)) in leaves.iter().zip(&before).enumerate() {
        if let Some(flag) = dirty.get_mut(i) {
            *flag |= a != b;
        }
    }
    // (A node that is gone is not written.)
    for (flag, n) in dirty.iter_mut().zip(&leaves).skip(1) {
        *flag &= *n != 0;
    }
    write_nodes(&mut ctx.objects, &tree, &dirty, &leaves);
    // The form loses the fields whose widgets were on the pages that went (12.7); they stay in the file, nobody lists them.
    form_after_delete(ctx, pages, &gone, catalog)?;
    // Page labels count pages by their place, and article threads list pages (12.4.3): both are right only if no page after the
    // ones taken out moves; threads only if no page goes.
    let tail = which.iter().enumerate().all(|(k, &p)| p as usize == pages.len() - which.len() + k);
    if !tail && catalog.remove("PageLabels") {
        ctx.notes.push(LABELS_GONE.to_string());
    }
    catalog.remove("Threads");
    Ok(())
}

const LABELS_GONE: &str = "the page labels were removed because the pages no longer line up with them";

/// Take the fields that have no widget left out of `/AcroForm`.
fn form_after_delete(ctx: &mut Ctx<'_>, pages: &[Page], gone: &[u32], catalog: &mut Catalog) -> Result<()> {
    let doc = ctx.doc;
    let Some(cut) = crate::prune::form_after_removal(doc, pages, gone)? else { return Ok(()) };
    ctx.objects.extend(cut.changed.into_iter().map(|(r, d)| (r.num, Object::Dict(d))));
    let Some(entry) = catalog.dict.get("AcroForm").cloned() else { return Ok(()) };
    let Object::Dict(mut acro) = doc.resolve(&entry)? else { return Ok(()) };
    if let Some(order) = cut.order {
        acro.set("CO", Object::Array(order));
    }
    if cut.fields.is_empty() {
        catalog.remove("AcroForm");
        return Ok(());
    }
    acro.set("Fields", Object::Array(cut.fields));
    put_form(ctx, catalog, &entry, acro);
    Ok(())
}

/// Write the form where it was: in the object `entry` names, or in the catalog.
fn put_form(ctx: &mut Ctx<'_>, catalog: &mut Catalog, entry: &Object, acro: Dict) {
    match entry {
        Object::Ref(r) if matches!(ctx.doc.get(*r), Ok(Object::Dict(_))) => ctx.objects.push((r.num, Object::Dict(acro))),
        _ => catalog.set("AcroForm", Object::Dict(acro)),
    }
}

fn move_pages(ctx: &mut Ctx<'_>, mut tree: Tree, pages: &[Page], which: &[u32], to: usize, catalog: &mut Catalog) -> Result<()> {
    let which = pick(which, pages.len())?;
    if which.is_empty() || to > pages.len() {
        return Err(odd("there is no such page"));
    }
    let order = order_after_move(pages.len(), &which, to);
    if order.iter().enumerate().all(|(i, &p)| p as usize == i) {
        return Err(odd("the pages are where they are to go"));
    }
    let mut dirty = vec![false; tree.nodes.len()];
    let mut shared: Vec<(usize, ObjRef)> = Vec::new();
    // Place `j` of the tree gets the page that was `from`; the node of that page's old place says whether it changes nodes.
    let places: Vec<(usize, usize, ObjRef)> = tree.slots.iter().map(|s| (s.node, s.at, s.page)).collect();
    for ((node, at, old), &from) in places.iter().zip(&order) {
        let (Some(page), Some(was)) = (pages.get(from as usize), tree.slots.get(from as usize)) else { continue };
        if page.obj_ref.num == old.num {
            continue;
        }
        let from_node = was.node;
        if let Some(kid) = tree.nodes.get_mut(*node).and_then(|n| n.kids.get_mut(*at)) {
            *kid = Object::Ref(page.obj_ref);
        }
        if let Some(flag) = dirty.get_mut(*node) {
            *flag = true;
        }
        if from_node != *node {
            rehome(ctx, &tree, page.obj_ref, from_node, *node, &mut shared)?;
        }
    }
    let leaves = tree.leaves(&sorted_nums(pages.iter().map(|p| p.obj_ref.num)));
    write_nodes(&mut ctx.objects, &tree, &dirty, &leaves);
    if catalog.remove("PageLabels") {
        ctx.notes.push(LABELS_GONE.to_string());
    }
    Ok(())
}

/// A page now listed by node `to` instead of node `from`: its `/Parent`, and what it inherited from the nodes above `from` written on
/// it, so that the nodes above `to` change nothing about it (7.7.3.4).
fn rehome(ctx: &mut Ctx<'_>, tree: &Tree, page: ObjRef, from: usize, to: usize, shared: &mut Vec<(usize, ObjRef)>) -> Result<()> {
    let Some(target) = tree.nodes.get(to) else { return Ok(()) };
    let mut dict = dict_of(ctx.doc, page)?;
    dict.set("Parent", Object::Ref(target.obj));
    for key in INHERITED {
        if dict.contains_key(key) {
            continue;
        }
        match (tree.inherited(from, key), tree.inherited(to, key)) {
            (Some((owner, value)), _) => {
                let value = shared_value(ctx, key, owner, value, shared)?;
                dict.set(key, value);
            }
            // The page had nothing, and the nodes above its new place would give it something: say what it was.
            (None, Some(_)) => {
                let neutral = match key {
                    "Resources" => Object::Dict(Dict::new()),
                    "Rotate" => Object::Integer(0),
                    // A crop box that is not there is the media box (14.11.2).
                    "CropBox" => dict.get("MediaBox").cloned().or_else(|| tree.inherited(from, "MediaBox").map(|(_, v)| v.clone())).unwrap_or_else(default_box),
                    _ => default_box(),
                };
                dict.set(key, neutral);
            }
            (None, None) => {}
        }
    }
    ctx.objects.push((page.num, Object::Dict(dict)));
    Ok(())
}

fn default_box() -> Object {
    Object::Array([0, 0, 612, 792].into_iter().map(Object::Integer).collect())
}

/// The value of an inherited entry to write on a page: a direct `/Resources` that many pages share becomes an object of its own, once.
fn shared_value(ctx: &mut Ctx<'_>, key: &str, owner: usize, value: &Object, shared: &mut Vec<(usize, ObjRef)>) -> Result<Object> {
    if key != "Resources" || !matches!(value, Object::Dict(_)) {
        return Ok(value.clone());
    }
    if let Some((_, known)) = shared.iter().find(|(o, _)| *o == owner) {
        return Ok(Object::Ref(*known));
    }
    let r = ctx.alloc()?;
    ctx.objects.push((r.num, value.clone()));
    shared.push((owner, r));
    Ok(Object::Ref(r))
}

// --- pages from another file ----------------------------------------------------------------------------------------------------

fn insert(ctx: &mut Ctx<'_>, mut tree: Tree, pages: &[Page], at: usize, path: &str, password: &Password, catalog: &mut Catalog) -> Result<()> {
    let doc = ctx.doc;
    if at > pages.len() {
        return Err(odd("there is no such page"));
    }
    // The file is read up to what can be put in, and one byte more to tell that it is bigger (what a file's length said a moment ago does
    // not bind what it is when it is read): that is the room the changes have left, and no more than the file may be at all.
    let limit = MAX_INSERT_FILE_BYTES.min(ctx.room.saturating_sub(ctx.used) as u64);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(std::fs::File::open(path)?, limit.saturating_add(1)), &mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Limit(TOO_BIG.to_string()));
    }
    let size = bytes.len();
    let source = Document::from_bytes_with_password(bytes, password.as_str())?;
    if source.is_locked() {
        return Err(Error::PasswordRequired);
    }
    // Pages of a file that restricts what may be done with it do not go into another file, as `merge` has it, unless it has the
    // protection of this file (a part split from it).
    let alike = doc.security().zip(source.security()).is_some_and(|(a, b)| a.same_protection(b));
    if !alike && ops::restricts(&source, password.as_str()) {
        return Err(odd("that file restricts what can be done with it: the owner password is needed to put its pages in another file"));
    }
    let source_pages = source.pages()?;
    if source_pages.is_empty() {
        return Err(odd("that file has no pages"));
    }
    if source_pages.len() > MAX_INSERT_PAGES {
        return Err(Error::Limit(format!("a file of more than {MAX_INSERT_PAGES} pages is not put in at once")));
    }
    if pages.len() + source_pages.len() > MAX_PAGES_AFTER {
        return Err(Error::Limit(format!("a document may have {MAX_PAGES_AFTER} pages at most")));
    }
    if !ctx.work.charge(IMPORT_BYTE_WORK * size as f64) {
        return Err(Error::Limit("too much has been put in during this session: save the file and open it again".to_string()));
    }

    // Where the pages go: next to the page they are put before (or after the last).
    let last = at == pages.len();
    let Some(place) = at.checked_sub(usize::from(last)).and_then(|i| tree.slots.get(i)).map(|s| (s.node, s.at + usize::from(last))) else {
        return Err(odd("the document has no pages"));
    };
    let parent = tree.nodes.get(place.0).map(|n| n.obj).ok_or_else(|| odd("the page tree is not as it was read"))?;

    let page_nums: HashSet<u32> = source_pages.iter().map(|p| p.obj_ref.num).collect();
    let mut b = Builder::collecting(ctx.next, ctx.room.saturating_sub(ctx.used), TOO_BIG);
    let s = b.add_source(&source, &page_nums);
    // The links that name a destination get the destination itself (the names of the other file are not here), and what points into
    // its structure tree goes: as in a merge.
    b.resolve_named_destinations(s);
    b.drop_structure_links(s);
    let fresh: Vec<ObjRef> = source_pages.iter().map(|_| b.reserve()).collect::<Result<_>>()?;
    for (page, new) in source_pages.iter().zip(&fresh) {
        b.register_page(s, page.obj_ref, *new);
    }
    let form = prepare_form(doc, &source, &mut b, s, ctx.work)?;
    for (page, new) in source_pages.iter().zip(&fresh) {
        let mut dict = ops::import_page(&mut b, s, page)?;
        dict.set("Parent", Object::Ref(parent));
        // The nodes above the place may give the page a crop box or a turn that the page did not have in its own file.
        for (key, neutral) in [("CropBox", dict.get("MediaBox").cloned().unwrap_or_else(default_box)), ("Rotate", Object::Integer(0))] {
            if !dict.contains_key(key) && tree.inherited(place.0, key).is_some() {
                dict.set(key, neutral);
            }
        }
        b.put(*new, &Object::Dict(dict))?;
    }
    if let Some(form) = form {
        merge_form(ctx, &source, &mut b, s, form, catalog)?;
    }
    let (objects, warnings) = b.into_collected()?;
    ctx.next = objects.iter().map(|(n, _)| n.saturating_add(1)).fold(ctx.next, u32::max);
    ctx.notes.extend(warnings.into_iter().map(|w| w.0));
    ctx.objects.extend(objects);

    // The tree: the pages are kids of the node next to them, and the counts above grow.
    let before = tree.leaves(&sorted_nums(pages.iter().map(|p| p.obj_ref.num)));
    let Some(node) = tree.nodes.get_mut(place.0) else { return Err(odd("the page tree is not as it was read")) };
    let at_kid = place.1.min(node.kids.len());
    drop(node.kids.splice(at_kid..at_kid, fresh.iter().map(|r| Object::Ref(*r))));
    let mut dirty = vec![false; tree.nodes.len()];
    let mut leaves = before;
    let mut up = Some(place.0);
    for _ in 0..=MAX_TREE_DEPTH {
        let Some(i) = up else { break };
        if let (Some(flag), Some(n)) = (dirty.get_mut(i), leaves.get_mut(i)) {
            *flag = true;
            *n += fresh.len();
        }
        up = tree.nodes.get(i).and_then(|n| n.parent);
    }
    write_nodes(&mut ctx.objects, &tree, &dirty, &leaves);

    if at < pages.len() && catalog.remove("PageLabels") {
        ctx.notes.push(LABELS_GONE.to_string());
    }
    if source.version() > doc.version() {
        let (major, minor) = source.version();
        catalog.set("Version", Object::Name(Name::new(format!("{major}.{minor}"))));
    }
    if source.is_encrypted() && !doc.is_encrypted() {
        ctx.notes.push("the file the pages come from is encrypted, and the pages are not in this file".to_string());
    }
    Ok(())
}

/// The form of the file pages come from: its dictionary and its fields of the first level, as the references the file has.
struct SourceForm {
    acro: Dict,
    fields: Vec<ObjRef>,
}

/// Look at the form of the other file. The fields of the first level whose names this document has already take other names (a field with
/// a name the document has would be that field: they would share a value), and the signatures its fields hold stay behind: they were made
/// over the bytes of the other file and say nothing of this one, and a file with a signature in it counts as signed, so that no page in it
/// could be changed any more. The field stays, unsigned: without its value, and without the picture of the signature on its widgets.
fn prepare_form(doc: &Document, source: &Document, b: &mut Builder<'_>, s: usize, work: &Work) -> Result<Option<SourceForm>> {
    let Some(entry) = source.catalog()?.get("AcroForm").cloned() else { return Ok(None) };
    let Ok(Object::Dict(acro)) = source.resolve(&entry) else { return Ok(None) };
    let fields: Vec<ObjRef> = match acro.get("Fields").map(|f| source.resolve(f)) {
        Some(Ok(Object::Array(items))) => items.iter().filter_map(Object::as_obj_ref).collect(),
        _ => Vec::new(),
    };
    if fields.is_empty() {
        return Ok(None);
    }
    let too_big = || Error::Limit("the form of that file has too many fields to put in".to_string());
    if fields.len() > MAX_FORM_FIELDS || !work.charge(FIELD_WORK * fields.len() as f64) {
        return Err(too_big());
    }
    // The names of the fields of the first level of this document and of the other file.
    let names = |d: &Document, roots: &[Object]| -> HashSet<Vec<u8>> {
        roots.iter().filter_map(Object::as_obj_ref).filter_map(|r| match d.get(r) {
            Ok(Object::Dict(f)) => field_name(&f),
            _ => None,
        }).collect()
    };
    let ours = match doc.catalog()?.get("AcroForm").and_then(|f| doc.resolve(f).ok()) {
        Some(Object::Dict(a)) => match a.get("Fields").map(|f| doc.resolve(f)) {
            Some(Ok(Object::Array(roots))) => names(doc, &roots),
            _ => HashSet::new(),
        },
        _ => HashSet::new(),
    };
    let theirs = names(source, &fields.iter().map(|r| Object::Ref(*r)).collect::<Vec<_>>());
    // A name the document has is changed to the first `name_n` that neither has: found once for a name (two fields of the other file that
    // had one name get the same new one: they were one field there).
    let mut renames: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let mut seen: HashSet<u32> = HashSet::new();
    // The fields still to look at, each with: is it of the first level, does `/FT /Sig` come from above, is a signature above it.
    let mut todo: Vec<(ObjRef, bool, bool, bool)> = fields.iter().map(|r| (*r, true, false, false)).collect();
    while let Some((r, root, sig, signed)) = todo.pop() {
        if !seen.insert(r.num) {
            continue;
        }
        if seen.len() > MAX_FORM_FIELDS || !work.charge(FIELD_WORK) {
            return Err(too_big());
        }
        let Ok(Object::Dict(mut d)) = source.get(r) else { continue };
        let mut changed = false;
        if root && let Some(name) = field_name(&d).filter(|n| ours.contains(n)) {
            let fresh = renames.entry(name.clone()).or_insert_with(|| (2u32..).map(|n| renamed(&name, n)).find(|c| !ours.contains(c) && !theirs.contains(c)).unwrap_or_else(|| name.clone()));
            d.set("T", Object::String(crate::object::PdfString::hex(fresh.clone())));
            changed = true;
        }
        let sig = match d.get("FT").and_then(|t| source.resolve(t).ok()) {
            Some(Object::Name(n)) => n == "Sig",
            _ => sig,
        };
        let unsigned = sig && d.remove("V").is_some();
        let signed = signed || unsigned;
        let dropped_look = signed && d.remove("AP").is_some();
        changed |= unsigned || dropped_look;
        if let Some(Ok(Object::Array(kids))) = d.get("Kids").map(|k| source.resolve(k)) {
            todo.extend(kids.iter().filter_map(Object::as_obj_ref).map(|k| (k, false, sig, signed)));
        }
        if changed {
            b.replace(s, r.num, Object::Dict(d));
        }
    }
    Ok(Some(SourceForm { acro, fields }))
}

/// The entry of `d` with this name (a name need not be text, so `Dict::get`, which takes a `&str`, is not used for it).
fn entry_named<'d>(d: &'d Dict, name: &Name) -> Option<&'d Object> {
    d.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
}

fn field_name(d: &Dict) -> Option<Vec<u8>> {
    match d.get("T") {
        Some(Object::String(s)) => Some(s.bytes.clone()),
        _ => None,
    }
}

/// The name `name` with `_n` after it, in the encoding the name has (text strings are PDFDocEncoding or UTF-16, 7.9.2.2).
fn renamed(name: &[u8], n: u32) -> Vec<u8> {
    let suffix = format!("_{n}");
    let mut out = name.to_vec();
    if name.starts_with(&[0xFE, 0xFF]) {
        out.extend(suffix.encode_utf16().flat_map(u16::to_be_bytes));
    } else if name.starts_with(&[0xFF, 0xFE]) {
        out.extend(suffix.encode_utf16().flat_map(u16::to_le_bytes));
    } else {
        out.extend_from_slice(suffix.as_bytes());
    }
    out
}

/// Put the fields of the other file into this document's form (`/AcroForm` is made if there is none): its fields, the resources they
/// are drawn with and its default appearance.
fn merge_form(ctx: &mut Ctx<'_>, source: &Document, b: &mut Builder<'_>, s: usize, form: SourceForm, catalog: &mut Catalog) -> Result<()> {
    let doc = ctx.doc;
    let mut roots: Vec<Object> = Vec::new();
    for r in &form.fields {
        roots.extend(b.import_ref(s, *r)?.map(Object::Ref));
    }
    // The resources the fields are drawn with, by name: their fonts and so on, each reference turned into this document's. A form with
    // more of them than `MAX_DR_ENTRIES` brings none (its fields have their own /DA), and what is brought is counted against the room
    // before it is merged.
    let mut kinds: Vec<(Name, Object)> = Vec::new();
    let mut count = 0usize;
    if let Some(Ok(Object::Dict(dr))) = form.acro.get("DR").map(|d| source.resolve(d)) {
        for kind in RESOURCE_KINDS {
            if let Some(Ok(Object::Dict(entries))) = dr.get(kind).map(|v| source.resolve(v)) {
                count += entries.len();
                kinds.push((Name::from(kind), Object::Dict(entries)));
            }
        }
    }
    if count > MAX_DR_ENTRIES {
        kinds.clear();
    }
    let Object::Dict(theirs) = b.import(s, Object::Dict(Dict::from_pairs(kinds)))? else { return Ok(()) };
    ctx.spend(theirs.approx_size())?;
    let entry = catalog.dict.get("AcroForm").cloned();
    let mut acro = match entry.as_ref().map(|e| doc.resolve(e)) {
        Some(Ok(Object::Dict(d))) => d,
        _ => Dict::new(),
    };
    let mut fields = match acro.get("Fields").map(|f| doc.resolve(f)) {
        Some(Ok(Object::Array(items))) => items,
        _ => Vec::new(),
    };
    fields.extend(roots);
    acro.set("Fields", Object::Array(fields));
    let mut resources = match acro.get("DR").map(|d| doc.resolve(d)) {
        Some(Ok(Object::Dict(d))) => d,
        _ => Dict::new(),
    };
    for (category, entries) in theirs.iter() {
        let Object::Dict(entries) = entries else { continue };
        let ours = match entry_named(&resources, category).map(|c| doc.resolve(c)) {
            Some(Ok(Object::Dict(d))) => d,
            _ => Dict::new(),
        };
        // The entries this document does not have (the ones it has stay as they are), added in one go.
        let have: HashSet<&Name> = ours.iter().map(|(n, _)| n).collect();
        let new: Vec<(Name, Object)> = entries.iter().filter(|(n, _)| !have.contains(n)).map(|(n, v)| (n.clone(), v.clone())).collect();
        if !new.is_empty() {
            let mut all = ours.into_pairs();
            all.extend(new);
            resources.set(category.clone(), Object::Dict(Dict::from_pairs(all)));
        }
    }
    if !resources.is_empty() {
        acro.set("DR", Object::Dict(resources));
    }
    for key in ["DA", "Q"] {
        if !acro.contains_key(key)
            && let Some(v) = form.acro.get(key)
        {
            acro.set(key, v.clone());
        }
    }
    if matches!(form.acro.get("NeedAppearances"), Some(Object::Bool(true))) {
        acro.set("NeedAppearances", Object::Bool(true));
    }
    put_form(ctx, catalog, &entry.unwrap_or(Object::Null), acro);
    Ok(())
}
