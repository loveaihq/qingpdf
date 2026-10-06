//! Checks on public corpus files for the findings of the layer 1 review: what
//! other readers do with a page tree that lists a page twice, a stray token in
//! a dictionary, and the links of a file merged with itself.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::collections::HashSet;

use qingpdf_core::{Document, ObjRef, Object, ops};

fn open(rel: &str) -> Document {
    Document::open(common::public_file(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Chrome (PDFium) and MuPDF both count a page the tree lists twice, and a
/// subtree reachable twice, as many times as it is listed.
#[test]
fn a_page_tree_that_lists_pages_or_subtrees_twice_counts_every_listing() {
    assert_eq!(open("page-tree/bug_1506.pdf").page_count().unwrap(), 4);
    assert_eq!(open("page-tree/no_page_count.pdf").page_count().unwrap(), 6);
    // A loop through the tree is cut, not expanded.
    assert_eq!(open("page-tree/Pages-tree-refs.pdf").page_count().unwrap(), 1);
    // And such a file is written out with every listing as a page of its own.
    for (rel, count) in [("page-tree/bug_1506.pdf", 4), ("page-tree/no_page_count.pdf", 6)] {
        let doc = open(rel);
        let out = ops::copy_all(&doc).unwrap();
        assert_eq!(out.pages, count, "{rel}");
        let copy = Document::from_bytes(out.data).unwrap();
        assert!(!copy.was_repaired(), "{rel}");
        let pages = copy.pages().unwrap();
        assert_eq!(pages.len(), count, "{rel}");
        let distinct: HashSet<ObjRef> = pages.iter().map(|p| p.obj_ref).collect();
        assert_eq!(distinct.len(), count, "{rel}: every listing is a page object of its own");
    }
}

/// A stray token such as `"72AF..."` in a dictionary used to make the whole
/// object unreadable, taking the attachment's data with it.
#[test]
fn an_attachment_with_a_stray_token_in_its_params_keeps_its_data() {
    let doc = open("outline-form-attach/embedded_attachments_with_desc.pdf");
    let Object::Stream(stream) = doc.get(ObjRef::new(10, 0)).unwrap() else { panic!("object 10 is not a stream") };
    assert_eq!(stream.data, b"345");
    assert!(stream.dict.get("Params").and_then(Object::as_dict).is_some_and(|p| !p.contains_key("CheckSum")));
    // It is still there after a copy.
    let out = ops::copy_all(&doc).unwrap();
    assert!(common::contains(&out.data, b"stream\n345\nendstream"), "the attachment's bytes are in the copy");
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
}

/// Round 2: the name tree written after pages are deleted must have indirect
/// references as /Kids at every level (ISO 32000-1 Table 36), or readers that
/// follow only references (MuPDF) find no destination at all.
#[test]
fn the_name_tree_written_after_deleting_pages_has_only_indirect_kids() {
    let doc = open("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf");
    let out = ops::delete_pages(&doc, &[2, 3, 4]).unwrap();
    let copy = Document::from_bytes(out.data).unwrap();
    let catalog = copy.catalog().unwrap();
    let names = copy.resolve(catalog.get("Names").unwrap()).unwrap();
    let root = names.as_dict().unwrap().get("Dests").unwrap().clone();
    assert!(matches!(root, Object::Ref(_)), "the root of the tree is an indirect object");
    let (mut nodes, mut leaves_entries) = (0usize, 0usize);
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        nodes += 1;
        let Object::Dict(d) = copy.resolve(&node).unwrap() else { panic!("a node is not a dictionary") };
        if let Some(kids) = d.get("Kids") {
            for kid in copy.resolve(kids).unwrap().as_array().unwrap() {
                assert!(matches!(kid, Object::Ref(_)), "a kid that is not an indirect reference: {kid:?}");
                stack.push(kid.clone());
            }
        }
        if let Some(pairs) = d.get("Names") {
            leaves_entries += copy.resolve(pairs).unwrap().as_array().unwrap().len() / 2;
        }
    }
    assert!(nodes >= 2 && leaves_entries > 100, "{nodes} nodes, {leaves_entries} entries");
}

/// Review finding 1: merging a LaTeX paper (links through named destinations)
/// with itself must not send the links of the second copy into the first.
#[test]
fn merging_a_paper_with_itself_keeps_the_links_of_the_second_copy_in_the_second_copy() {
    let doc = open("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf");
    let count = doc.page_count().unwrap();
    let out = ops::merge(&[
        ops::Input { name: "a.pdf", doc: &doc },
        ops::Input { name: "a again.pdf", doc: &doc },
    ])
    .unwrap();
    let merged = Document::from_bytes(out.data).unwrap();
    let pages = merged.pages().unwrap();
    assert_eq!(pages.len(), 2 * count);
    let first_half: HashSet<ObjRef> = pages[..count].iter().map(|p| p.obj_ref).collect();
    let second_half: HashSet<ObjRef> = pages[count..].iter().map(|p| p.obj_ref).collect();
    let (mut into_second, mut into_first, mut unresolved) = (0usize, 0usize, 0usize);
    for page in &pages[count..] {
        let Some(annots) = page.dict.get("Annots").and_then(|a| merged.resolve(a).ok()) else { continue };
        for annot in annots.as_array().unwrap_or(&[]) {
            let Object::Dict(annot) = merged.resolve(annot).unwrap() else { continue };
            let dest = match (annot.get("Dest"), annot.get("A")) {
                (Some(d), _) => Some(merged.resolve(d).unwrap()),
                (None, Some(a)) => {
                    let Object::Dict(action) = merged.resolve(a).unwrap() else { continue };
                    action.get("D").map(|d| merged.resolve(d).unwrap())
                }
                _ => None,
            };
            match dest {
                Some(Object::Array(items)) => match items.first() {
                    Some(Object::Ref(r)) if second_half.contains(r) => into_second += 1,
                    Some(Object::Ref(r)) if first_half.contains(r) => into_first += 1,
                    _ => {}
                },
                Some(Object::String(_) | Object::Name(_)) => unresolved += 1,
                _ => {}
            }
        }
    }
    assert!(into_second > 100, "the second copy has its own internal links ({into_second})");
    assert_eq!(into_first, 0, "no link of the second copy leads into the first");
    assert_eq!(unresolved, 0, "no link of the second copy is left naming a destination of the first");
}
