//! The changes to the pages (`pages.rs`) on small files made by hand: the objects they write, the page tree after them, undoing and
//! redoing, saving as an update and reading it again.

use super::*;
use crate::render::work::Work;
use crate::testutil::PdfBuilder;

const NOW: &str = "D:20260101000000Z";

/// `n` pages in one node, page `i` (object `10 + i`) `100 + i` points wide.
fn flat(n: usize) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..n).map(|i| format!("{} 0 R ", 10 + i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>"));
    for i in 0..n {
        b.obj(10 + i as u32, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 200] /Resources << >> >>", 100 + i));
    }
    b.finish_classic(10 + n as u32, "/Root 1 0 R")
}

/// Five pages in two nodes (20: pages 10, 11, 12; 21: pages 13, 14), the nodes saying what the pages inherit: node 20 a turn of 90
/// degrees and a crop box, node 21 a media box of its own.
fn nested() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [20 0 R 21 0 R] /Count 5 /MediaBox [0 0 500 500] /Resources << /ProcSet [/PDF] >> >>");
    b.obj(20, "<< /Type /Pages /Parent 2 0 R /Kids [10 0 R 11 0 R 12 0 R] /Count 3 /Rotate 90 /CropBox [0 0 400 400] >>");
    b.obj(21, "<< /Type /Pages /Parent 2 0 R /Kids [13 0 R 14 0 R] /Count 2 /MediaBox [0 0 700 700] >>");
    for i in 0..3u32 {
        b.obj(10 + i, &format!("<< /Type /Page /Parent 20 0 R /Contents {} 0 R >>", 30 + i));
    }
    for i in 3..5u32 {
        b.obj(10 + i, &format!("<< /Type /Page /Parent 21 0 R /Contents {} 0 R >>", 30 + i));
    }
    for i in 0..5u32 {
        b.stream_obj(30 + i, "", format!("{i}").as_bytes());
    }
    b.finish_classic(40, "/Root 1 0 R")
}

fn run(doc: &Document, op: PageOp) -> Result<Vec<Change>> {
    let pages = doc.pages()?;
    let (objects, _) = apply_any(doc, &pages, &Edit::Pages(Box::new(op)), NOW, &Work::new(), DEFAULT_ROOM)?;
    Ok(commit(doc, objects))
}

fn widths(doc: &Document) -> Vec<i64> {
    doc.pages().unwrap().iter().map(|p| p.media_box().map_or(-1, |b| b[2] as i64)).collect()
}

fn nums(doc: &Document) -> Vec<u32> {
    doc.pages().unwrap().iter().map(|p| p.obj_ref.num).collect()
}

fn rotations(doc: &Document) -> Vec<i64> {
    doc.pages().unwrap().iter().map(Page::rotate).collect()
}

/// The pages as the file says them after `doc`'s changes are written as an update to `base` and read again (not repaired).
fn saved(base: &[u8], doc: &Document) -> Document {
    let mut bytes = base.to_vec();
    bytes.extend_from_slice(&save_section(doc).unwrap());
    let again = Document::from_bytes(bytes).unwrap();
    assert!(!again.was_repaired());
    assert_eq!(again.page_count().unwrap(), doc.page_count().unwrap());
    again
}

fn count_of(doc: &Document, node: u32) -> Option<i64> {
    doc.get(ObjRef::new(node, 0)).unwrap().as_dict().unwrap().get_int("Count")
}

fn kids_of_node(doc: &Document, node: u32) -> Vec<u32> {
    let d = doc.get(ObjRef::new(node, 0)).unwrap();
    d.as_dict().unwrap().get("Kids").unwrap().as_array().unwrap().iter().filter_map(|k| k.as_obj_ref().map(|r| r.num)).collect()
}

#[test]
fn pages_are_turned_and_the_turn_is_kept_off_a_page_that_gets_it_from_above() {
    let base = flat(4);
    let doc = Document::from_bytes(base.clone()).unwrap();
    run(&doc, PageOp::Rotate { pages: vec![2, 0, 2], degrees: 90 }).unwrap();
    assert_eq!(rotations(&doc), [90, 0, 90, 0]);
    run(&doc, PageOp::Rotate { pages: vec![0], degrees: -90 }).unwrap();
    run(&doc, PageOp::Rotate { pages: vec![2], degrees: 180 }).unwrap();
    assert_eq!(rotations(&doc), [0, 0, 270, 0]);
    // A page turned back to upright has no /Rotate left to write (and the page tree is not touched by turning).
    let dict = doc.get(ObjRef::new(10, 0)).unwrap();
    assert!(!dict.as_dict().unwrap().contains_key("Rotate"));
    assert_eq!(kids_of_node(&doc, 2).len(), 4);
    let again = saved(&base, &doc);
    assert_eq!(rotations(&again), [0, 0, 270, 0]);
    // Nothing to turn, a turn that is not a quarter, a page that is not there: refused, and nothing is written.
    let before = doc.overlay_entries().len();
    for op in [
        PageOp::Rotate { pages: vec![], degrees: 90 },
        PageOp::Rotate { pages: vec![1], degrees: 360 },
        PageOp::Rotate { pages: vec![1], degrees: 45 },
        PageOp::Rotate { pages: vec![9], degrees: 90 },
    ] {
        assert!(run(&doc, op.clone()).is_err(), "{op:?}");
    }
    assert_eq!(doc.overlay_entries().len(), before);
}

#[test]
fn a_page_whose_turn_comes_from_a_node_gets_its_own_turn_when_the_effective_turn_differs() {
    let base = nested();
    let doc = Document::from_bytes(base).unwrap();
    assert_eq!(rotations(&doc), [90, 90, 90, 0, 0]);
    // Back to upright: the node says 90, so the page must say 0 itself.
    run(&doc, PageOp::Rotate { pages: vec![1], degrees: 270 }).unwrap();
    assert_eq!(rotations(&doc), [90, 0, 90, 0, 0]);
    assert_eq!(doc.get(ObjRef::new(11, 0)).unwrap().as_dict().unwrap().get_int("Rotate"), Some(0));
    // Turned 90 again it equals what the node gives: no entry of its own.
    run(&doc, PageOp::Rotate { pages: vec![1], degrees: 90 }).unwrap();
    assert!(!doc.get(ObjRef::new(11, 0)).unwrap().as_dict().unwrap().contains_key("Rotate"));
    assert_eq!(rotations(&doc), [90, 90, 90, 0, 0]);
}

#[test]
fn deleting_pages_takes_them_out_of_the_kids_and_puts_the_counts_right_and_the_undo_brings_them_back() {
    let base = flat(5);
    let doc = Document::from_bytes(base.clone()).unwrap();
    let first = run(&doc, PageOp::Delete { pages: vec![3, 1] }).unwrap();
    assert_eq!(nums(&doc), [10, 12, 14]);
    assert_eq!(count_of(&doc, 2), Some(3));
    let again = saved(&base, &doc);
    assert_eq!(widths(&again), [100, 102, 104]);
    // The deleted pages are still objects of the file; nothing lists them.
    assert!(again.get(ObjRef::new(11, 0)).unwrap().as_dict().is_some());
    replay(&doc, &first, false);
    assert_eq!(widths(&doc), [100, 101, 102, 103, 104]);
    assert!(!doc.has_overlay());
    replay(&doc, &first, true);
    assert_eq!(widths(&doc), [100, 102, 104]);
    // Not every page, not a page that is not there, not nothing.
    for op in [PageOp::Delete { pages: vec![0, 1, 2] }, PageOp::Delete { pages: vec![7] }, PageOp::Delete { pages: vec![] }] {
        assert!(run(&doc, op.clone()).is_err(), "{op:?}");
    }
}

#[test]
fn a_node_left_without_pages_goes_and_the_counts_above_it_are_right() {
    let base = nested();
    let doc = Document::from_bytes(base.clone()).unwrap();
    // Both pages of node 21 go: the node is taken out of the root.
    run(&doc, PageOp::Delete { pages: vec![3, 4] }).unwrap();
    assert_eq!(nums(&doc), [10, 11, 12]);
    assert_eq!(kids_of_node(&doc, 2), [20]);
    assert_eq!(count_of(&doc, 2), Some(3));
    assert_eq!(count_of(&doc, 20), Some(3));
    let again = saved(&base, &doc);
    assert_eq!(nums(&again), [10, 11, 12]);
    // One page of node 20 goes: its count and the root's change, the node stays.
    let doc = Document::from_bytes(base.clone()).unwrap();
    run(&doc, PageOp::Delete { pages: vec![1] }).unwrap();
    assert_eq!((count_of(&doc, 20), count_of(&doc, 21), count_of(&doc, 2)), (Some(2), Some(2), Some(4)));
    assert_eq!(nums(&doc), [10, 12, 13, 14]);
    // What the pages inherit is the same as it was.
    assert_eq!(rotations(&doc), [90, 90, 0, 0]);
    assert_eq!(widths(&doc), [500, 500, 700, 700]);
}

#[test]
fn the_order_after_a_move_is_the_pages_put_in_front_of_the_place_in_their_order() {
    assert_eq!(order_after_move(6, &[4, 1], 0), [1, 4, 0, 2, 3, 5]);
    assert_eq!(order_after_move(6, &[1, 2], 6), [0, 3, 4, 5, 1, 2]);
    assert_eq!(order_after_move(6, &[0], 3), [1, 2, 0, 3, 4, 5]);
    // In front of a page that is moved itself or next to the block: where it stays.
    assert_eq!(order_after_move(5, &[1, 2], 2), [0, 1, 2, 3, 4]);
    assert_eq!(order_after_move(5, &[1, 2], 3), [0, 1, 2, 3, 4]);
    assert_eq!(order_after_move(5, &[1, 2], 1), [0, 1, 2, 3, 4]);
    // Pages that are not there, or twice: left out, once.
    assert_eq!(order_after_move(3, &[2, 9, 2], 0), [2, 0, 1]);
    assert_eq!(order_after_move(0, &[0], 0), Vec::<u32>::new());
}

#[test]
fn pages_are_moved_in_a_flat_tree_by_writing_the_kids_once() {
    let base = flat(6);
    let doc = Document::from_bytes(base.clone()).unwrap();
    run(&doc, PageOp::Move { pages: vec![4, 1], to: 0 }).unwrap();
    assert_eq!(widths(&doc), [101, 104, 100, 102, 103, 105]);
    // Only the node is written (no page changes its node): the update has the root and nothing else.
    let numbers: Vec<u32> = doc.overlay_entries().iter().map(|e| e.0).collect();
    assert_eq!(numbers, [2]);
    let again = saved(&base, &doc);
    assert_eq!(widths(&again), [101, 104, 100, 102, 103, 105]);
    // To the end, and nowhere (the pages are where they are to go).
    run(&doc, PageOp::Move { pages: vec![0], to: 6 }).unwrap();
    assert_eq!(widths(&doc), [104, 100, 102, 103, 105, 101]);
    assert!(run(&doc, PageOp::Move { pages: vec![1, 2], to: 2 }).is_err());
    assert!(run(&doc, PageOp::Move { pages: vec![1], to: 9 }).is_err());
}

#[test]
fn a_page_that_moves_to_another_node_takes_what_it_inherited_along() {
    let base = nested();
    let doc = Document::from_bytes(base.clone()).unwrap();
    let (turn, size) = (rotations(&doc), widths(&doc));
    // Page 0 (node 20: turned 90, crop box 400, media box 500 from the root) goes to the end (node 21: media box 700).
    run(&doc, PageOp::Move { pages: vec![0], to: 5 }).unwrap();
    assert_eq!(nums(&doc), [11, 12, 13, 14, 10]);
    // The shape of the tree is the same: the nodes have the pages they had, in the places they had.
    assert_eq!((count_of(&doc, 20), count_of(&doc, 21)), (Some(3), Some(2)));
    assert_eq!(kids_of_node(&doc, 20), [11, 12, 13]);
    assert_eq!(kids_of_node(&doc, 21), [14, 10]);
    // ... and every page is what it was.
    let after = (rotations(&doc), widths(&doc));
    assert_eq!(after.0, [turn[1], turn[2], turn[3], turn[4], turn[0]]);
    assert_eq!(after.1, [size[1], size[2], size[3], size[4], size[0]]);
    let crops: Vec<Option<[f64; 4]>> = doc.pages().unwrap().iter().map(Page::crop_box).collect();
    // (Page 13 now sits under node 20, which crops: it has the crop box that is its media box, which is no crop; page 10 keeps its crop.)
    assert_eq!(crops, [Some([0.0, 0.0, 400.0, 400.0]), Some([0.0, 0.0, 400.0, 400.0]), Some([0.0, 0.0, 700.0, 700.0]), None, Some([0.0, 0.0, 400.0, 400.0])]);
    // The page that moved came from node 20 and went to node 21: its parent says so.
    assert_eq!(doc.get(ObjRef::new(10, 0)).unwrap().as_dict().unwrap().get("Parent"), Some(&Object::Ref(ObjRef::new(21, 0))));
    // The resources it got from the root are shared, not copied into it as a dictionary.
    let page = doc.get(ObjRef::new(10, 0)).unwrap();
    assert!(matches!(page.as_dict().unwrap().get("Resources"), Some(Object::Ref(_))), "{page:?}");
    let again = saved(&base, &doc);
    assert_eq!((rotations(&again), widths(&again)), after);
    // A page that moves from the node of a page without a crop box (node 21) to the other one is not cropped by it.
    let doc = Document::from_bytes(base).unwrap();
    run(&doc, PageOp::Move { pages: vec![4], to: 0 }).unwrap();
    assert_eq!(nums(&doc), [14, 10, 11, 12, 13]);
    let crops: Vec<Option<[f64; 4]>> = doc.pages().unwrap().iter().map(Page::crop_box).collect();
    assert_eq!(crops[0], Some([0.0, 0.0, 700.0, 700.0]), "it is under node 20 now, which crops, and it was not cropped: its crop box is its media box");
}

#[test]
fn pages_with_a_media_box_of_their_own_keep_it_when_they_move() {
    // Every page of node 21 has a size of its own; page 13 goes into node 20, whose pages are 500 wide.
    let base = nested();
    let doc = Document::from_bytes(base).unwrap();
    assert_eq!(widths(&doc), [500, 500, 500, 700, 700]);
    run(&doc, PageOp::Move { pages: vec![3], to: 0 }).unwrap();
    assert_eq!(widths(&doc), [700, 500, 500, 500, 700]);
    assert_eq!(doc.get(ObjRef::new(13, 0)).unwrap().as_dict().unwrap().get("Parent"), Some(&Object::Ref(ObjRef::new(20, 0))));
}

/// A file with a page of three annotations: a link to the other page by a name, a link to it by an explicit destination, a field.
fn source_file(field_name: &str, pages: usize) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 5 0 R >> /AcroForm << /Fields [8 0 R] /DR << /Font << /Helv 9 0 R >> >> /DA (/Helv 0 Tf 0 g) >> /Version /1.7 >>");
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 250 250] /Rotate 90 >>"));
    b.obj(5, "<< /Names [(second) [11 0 R /Fit]] >>");
    b.obj(8, &format!("<< /FT /Tx /T ({field_name}) /V (typed) /Rect [10 10 90 30] /Subtype /Widget /Type /Annot /P 10 0 R /AP << /N 7 0 R >> /DA (/Helv 12 Tf 0 g) >>"));
    b.obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.stream_obj(7, "/Type /XObject /Subtype /Form /BBox [0 0 80 20]", b"q Q");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /Annots [8 0 R 6 0 R 4 0 R] /StructParents 3 /Contents 3 0 R >>");
    b.obj(6, "<< /Type /Annot /Subtype /Link /Rect [0 0 20 20] /Dest (second) >>");
    b.obj(4, "<< /Type /Annot /Subtype /Link /Rect [30 0 50 20] /A << /S /GoTo /D [11 0 R /XYZ 0 0 0] >> >>");
    b.stream_obj(3, "", b"0 0 10 10 re f");
    for i in 1..pages {
        b.obj(10 + i as u32, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 333 222] >>");
    }
    b.finish_classic(10 + pages as u32, "/Root 1 0 R")
}

fn source_on_disk(name: &str, bytes: &[u8]) -> String {
    let dir = std::env::temp_dir().join(format!("qingpdf-pages-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

fn insert(at: u32, path: &str) -> PageOp {
    PageOp::Insert { at, path: path.to_string(), password: Password::default() }
}

fn annots(doc: &Document, page: usize) -> Vec<Dict> {
    let p = doc.pages().unwrap();
    let d = doc.get(p[page].obj_ref).unwrap();
    let list = doc.resolve(d.as_dict().unwrap().get("Annots").unwrap_or(&Object::Null)).unwrap();
    list.as_array().unwrap_or(&[]).iter().map(|a| doc.resolve(a).unwrap().as_dict().unwrap().clone()).collect()
}

#[test]
fn pages_from_a_file_come_in_with_their_annotations_fields_links_and_without_the_other_files_structure() {
    let base = flat(3);
    let doc = Document::from_bytes(base.clone()).unwrap();
    let path = source_on_disk("a.pdf", &source_file("Name", 2));
    run(&doc, insert(1, &path)).unwrap();
    assert_eq!(doc.page_count().unwrap(), 5);
    // Their sizes are theirs (the media box and the turn of the other file's page tree are written on them), the others are in place.
    assert_eq!(widths(&doc), [100, 250, 333, 101, 102]);
    assert_eq!(rotations(&doc), [0, 90, 90, 0, 0]);
    assert_eq!(count_of(&doc, 2), Some(5));
    let pages = doc.pages().unwrap();
    let new_first = pages[1].obj_ref;
    let new_second = pages[2].obj_ref;
    // The annotations came, and the links lead to the pages that came (the named one was turned into an explicit destination).
    let list = annots(&doc, 1);
    assert_eq!(list.len(), 3);
    let by_subtype = |s: &str| list.iter().filter(|d| d.get_name("Subtype").is_some_and(|n| n == s)).count();
    assert_eq!((by_subtype("Widget"), by_subtype("Link")), (1, 2));
    for d in list.iter().filter(|d| d.get_name("Subtype").is_some_and(|n| n == "Link")) {
        let dest = match d.get("Dest") {
            Some(Object::Array(a)) => a.clone(),
            _ => d.get("A").and_then(|a| a.as_dict()).and_then(|a| a.get("D")).and_then(Object::as_array).unwrap().to_vec(),
        };
        assert_eq!(dest[0], Object::Ref(new_second), "{d:?}");
    }
    // The widget is on the page that came, and its field is in the form, which the file did not have.
    let widget = list.iter().find(|d| d.get_name("Subtype").is_some_and(|n| n == "Widget")).unwrap();
    assert_eq!(widget.get("P"), Some(&Object::Ref(new_first)));
    let acro = doc.catalog().unwrap();
    let acro = doc.resolve(acro.get("AcroForm").unwrap()).unwrap();
    let fields = acro.as_dict().unwrap().get("Fields").and_then(Object::as_array).unwrap().to_vec();
    assert_eq!(fields.len(), 1);
    assert!(acro.as_dict().unwrap().get("DR").is_some() && acro.as_dict().unwrap().get("DA").is_some());
    // The structure tree numbers are not carried over; the version of the other file is.
    assert!(!doc.get(new_first).unwrap().as_dict().unwrap().contains_key("StructParents"));
    assert_eq!(doc.version(), (1, 7));
    // Written as an update and read again, it is the same.
    let again = saved(&base, &doc);
    assert_eq!(widths(&again), [100, 250, 333, 101, 102]);
    assert_eq!(annots(&again, 1).len(), 3);
    // Undone, it is gone.
    let undone = run(&Document::from_bytes(base).unwrap(), insert(3, &path)).unwrap();
    assert!(!undone.is_empty());
}

#[test]
fn a_field_whose_name_the_document_has_gets_another_name_and_the_fields_are_merged_into_the_form() {
    // The document has a form with a field "Name"; the file's field is "Name" too, and a second file is put in as well.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R] /DR << /Font << /Helv 9 0 R >> >> >> >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>");
    b.obj(8, "<< /FT /Tx /T (Name) /V (mine) /Rect [10 10 90 30] /Subtype /Widget /Type /Annot /P 10 0 R >>");
    b.obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [8 0 R] >>");
    let base = b.finish_classic(11, "/Root 1 0 R");
    let doc = Document::from_bytes(base.clone()).unwrap();
    let path = source_on_disk("b.pdf", &source_file("Name", 1));
    run(&doc, insert(1, &path)).unwrap();
    run(&doc, insert(2, &path)).unwrap();
    let acro = doc.resolve(doc.catalog().unwrap().get("AcroForm").unwrap()).unwrap();
    let fields: Vec<Dict> = acro.as_dict().unwrap().get("Fields").and_then(Object::as_array).unwrap().iter().map(|f| doc.resolve(f).unwrap().as_dict().unwrap().clone()).collect();
    let names: Vec<String> = fields.iter().map(|f| match f.get("T") {
        Some(Object::String(s)) => String::from_utf8_lossy(&s.bytes).into_owned(),
        _ => String::new(),
    }).collect();
    assert_eq!(names, ["Name", "Name_2", "Name_3"]);
    // The values are the fields' own.
    assert!(matches!(fields[0].get("V"), Some(Object::String(s)) if s.bytes == b"mine"));
    assert!(matches!(fields[1].get("V"), Some(Object::String(s)) if s.bytes == b"typed"));
    // The resources: the document's font by the name it has, nothing of the other file's overrides it.
    let helv = doc.resolve(acro.as_dict().unwrap().get("DR").unwrap()).unwrap();
    let font = doc.resolve(helv.as_dict().unwrap().get("Font").unwrap()).unwrap();
    let helv = doc.resolve(font.as_dict().unwrap().get("Helv").unwrap()).unwrap();
    assert_eq!(helv.as_dict().unwrap().get_name("BaseFont").unwrap().as_bytes(), b"Courier");
    let again = saved(&base, &doc);
    assert_eq!(again.page_count().unwrap(), 3);
}

#[test]
fn a_file_that_cannot_be_put_in_is_refused_and_nothing_is_written() {
    let doc = Document::from_bytes(flat(2)).unwrap();
    assert!(run(&doc, insert(0, "no-such-file.pdf")).is_err());
    let junk = source_on_disk("junk.pdf", b"this is not a pdf");
    assert!(run(&doc, insert(0, &junk)).is_err());
    let good = source_on_disk("c.pdf", &flat(2));
    assert!(run(&doc, insert(3, &good)).is_err(), "no such place");
    let none = source_on_disk("none.pdf", &{
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.finish_classic(3, "/Root 1 0 R")
    });
    assert!(run(&doc, insert(0, &none)).is_err(), "a file with no pages");
    // Too little room for what comes in: refused with the message the reader adds advice to.
    let pages = doc.pages().unwrap();
    let big = source_on_disk("big.pdf", &source_file("X", 40));
    let refused = apply_any(&doc, &pages, &Edit::Pages(Box::new(insert(0, &big))), NOW, &Work::new(), 200);
    assert!(matches!(&refused, Err(Error::Limit(m)) if m == TOO_BIG), "{:?}", refused.map(|_| ()));
    assert!(!doc.has_overlay());
}

#[test]
fn a_page_tree_that_lists_a_page_twice_or_a_node_twice_is_not_changed() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R 10 0 R 11 0 R] /Count 3 >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
    b.obj(11, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
    let twice = Document::from_bytes(b.finish_classic(12, "/Root 1 0 R")).unwrap();
    assert_eq!(twice.page_count().unwrap(), 3);
    assert!(run(&twice, PageOp::Rotate { pages: vec![0], degrees: 90 }).is_err());
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>");
    b.obj(3, "<< /Type /Pages /Parent 2 0 R /Kids [10 0 R] /Count 1 >>");
    b.obj(10, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 100 100] >>");
    let shared = Document::from_bytes(b.finish_classic(11, "/Root 1 0 R")).unwrap();
    assert!(run(&shared, PageOp::Delete { pages: vec![0] }).is_err());
    // A tree that contains itself ends in an error (the reader cuts the cycle; a change does not go on with a tree like that).
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 10 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Pages /Parent 2 0 R /Kids [2 0 R] /Count 0 >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
    let cyclic = Document::from_bytes(b.finish_classic(11, "/Root 1 0 R")).unwrap();
    assert!(run(&cyclic, PageOp::Rotate { pages: vec![0], degrees: 90 }).is_err());
}

#[test]
fn deleting_pages_takes_their_form_fields_out_of_the_form_and_the_labels_go_when_pages_shift() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [20 0 R 21 0 R] /CO [20 0 R 21 0 R] >> /PageLabels << /Nums [0 << /S /D >>] >> /Threads [] >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R 11 0 R 12 0 R] /Count 3 /MediaBox [0 0 100 100] >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /Annots [20 0 R] >>");
    b.obj(11, "<< /Type /Page /Parent 2 0 R /Annots [21 0 R] >>");
    b.obj(12, "<< /Type /Page /Parent 2 0 R >>");
    b.obj(20, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (one) /V (1) /Rect [0 0 9 9] /P 10 0 R >>");
    b.obj(21, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (two) /V (2) /Rect [0 0 9 9] /P 11 0 R >>");
    let base = b.finish_classic(30, "/Root 1 0 R");
    let doc = Document::from_bytes(base.clone()).unwrap();
    let first = run(&doc, PageOp::Delete { pages: vec![0] }).unwrap();
    let catalog = doc.catalog().unwrap();
    let acro = doc.resolve(catalog.get("AcroForm").unwrap()).unwrap();
    let fields = acro.as_dict().unwrap().get("Fields").and_then(Object::as_array).unwrap().to_vec();
    assert_eq!(fields, [Object::Ref(ObjRef::new(21, 0))]);
    assert_eq!(acro.as_dict().unwrap().get("CO").and_then(Object::as_array).unwrap().len(), 1);
    // The page after the one that went moved: the labels (by place) are not right any more, and the threads (which list pages) go.
    assert!(!catalog.contains_key("PageLabels") && !catalog.contains_key("Threads"));
    let again = saved(&base, &doc);
    assert!(!again.catalog().unwrap().contains_key("PageLabels"));
    replay(&doc, &first, false);
    assert!(doc.catalog().unwrap().contains_key("PageLabels"));
    // Pages from the end leave the labels alone (every page that stays is where it was), the field of the page that goes still goes.
    run(&doc, PageOp::Delete { pages: vec![1, 2] }).unwrap();
    assert!(doc.catalog().unwrap().contains_key("PageLabels"));
    let acro = doc.resolve(doc.catalog().unwrap().get("AcroForm").unwrap()).unwrap();
    assert_eq!(acro.as_dict().unwrap().get("Fields").and_then(Object::as_array).unwrap().to_vec(), [Object::Ref(ObjRef::new(20, 0))]);
    // The last field's page goes too: with no field left the form is gone.
    let doc = Document::from_bytes(base).unwrap();
    run(&doc, PageOp::Delete { pages: vec![0, 1] }).unwrap();
    assert!(!doc.catalog().unwrap().contains_key("AcroForm"));
}

#[test]
fn a_session_of_changes_is_held_to_its_allowance_of_work() {
    let doc = Document::from_bytes(flat(3)).unwrap();
    let pages = doc.pages().unwrap();
    let edit = Edit::Pages(Box::new(PageOp::Rotate { pages: vec![0], degrees: 90 }));
    assert!(matches!(apply_any(&doc, &pages, &edit, NOW, &Work::with_allowance(1.0), DEFAULT_ROOM), Err(Error::Limit(_))));
    let work = Work::new();
    apply_any(&doc, &pages, &edit, NOW, &work, DEFAULT_ROOM).unwrap();
    assert!(work.used() >= 5.0e6 && work.used() < 1.0e7, "{}", work.used());
}

#[test]
fn a_password_is_not_shown_and_a_page_op_is_a_page_class_change_with_no_page_of_its_own() {
    let op = insert(0, "x.pdf");
    let shown = format!("{op:?}");
    assert!(!shown.contains("secret"));
    let with = PageOp::Insert { at: 0, path: "x".to_string(), password: Password::new("secret") };
    assert!(!format!("{with:?}").contains("secret"));
    let edit = Edit::Pages(Box::new(op));
    assert_eq!((edit.page(), edit.class()), (ALL_PAGES, EditClass::Assemble));
    assert_eq!(PageOp::Delete { pages: vec![1, 1, 2, 9] }.count_after(5), Some(3));
    assert_eq!(PageOp::Rotate { pages: vec![1], degrees: 90 }.count_after(5), Some(5));
}

// --- the fixes after the first review ----------------------------------------------------------------------------------------------

/// A file with a signature field that is its own widget (`Sig1`), one with a widget of its own that takes the type from above (`Sig2`),
/// and a text field; both signature fields are signed by the same signature.
fn signed_source() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R 12 0 R 14 0 R] /SigFlags 3 >> >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>");
    b.obj(8, "<< /FT /Sig /T (Sig1) /V 20 0 R /Rect [0 0 100 30] /Subtype /Widget /Type /Annot /P 10 0 R /AP << /N 7 0 R >> >>");
    b.obj(12, "<< /FT /Sig /T (Sig2) /V 20 0 R /Kids [13 0 R] >>");
    b.obj(13, "<< /Parent 12 0 R /Rect [0 40 100 70] /Subtype /Widget /Type /Annot /P 10 0 R /AP << /N 7 0 R >> >>");
    b.obj(14, "<< /FT /Tx /T (Text) /V (kept) /Rect [0 80 100 100] /Subtype /Widget /Type /Annot /P 10 0 R >>");
    b.obj(20, "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /Contents <00> /ByteRange [0 0 0 0] >>");
    b.stream_obj(7, "/Type /XObject /Subtype /Form /BBox [0 0 100 30]", b"q Q");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Annots [8 0 R 13 0 R 14 0 R] >>");
    b.finish_classic(21, "/Root 1 0 R")
}

fn free_of_signatures(doc: &Document) -> bool {
    super::signature::look(doc).policy(EditClass::Assemble) == super::signature::Policy::Free
}

#[test]
fn a_signature_does_not_come_in_with_the_page_it_was_on_and_the_file_is_not_taken_for_signed() {
    let source = signed_source();
    assert!(!free_of_signatures(&Document::from_bytes(source.clone()).unwrap()), "the file we put in from is signed");
    let base = flat(2);
    let doc = Document::from_bytes(base.clone()).unwrap();
    run(&doc, insert(1, &source_on_disk("signed.pdf", &source))).unwrap();
    assert_eq!(doc.page_count().unwrap(), 3);
    assert!(free_of_signatures(&doc));
    // The fields are there, unsigned, and their widgets show no signature; the text field is what it was.
    let list = annots(&doc, 1);
    assert_eq!(list.len(), 3);
    for widget in &list[..2] {
        assert!(!widget.contains_key("AP") && !widget.contains_key("V"), "{widget:?}");
    }
    assert!(matches!(list[2].get("V"), Some(Object::String(s)) if s.bytes == b"kept"));
    let acro = doc.resolve(doc.catalog().unwrap().get("AcroForm").unwrap()).unwrap();
    let fields: Vec<Dict> = acro.as_dict().unwrap().get("Fields").and_then(Object::as_array).unwrap().iter().map(|f| doc.resolve(f).unwrap().as_dict().unwrap().clone()).collect();
    assert_eq!(fields.len(), 3);
    assert!(fields.iter().all(|f| !f.contains_key("V") || f.get_name("FT").is_some_and(|t| t == "Tx")), "{fields:?}");
    assert!(!acro.as_dict().unwrap().contains_key("SigFlags"));
    // Saved and read again it is not signed either: the pages can be changed some more.
    let again = saved(&base, &doc);
    assert!(free_of_signatures(&again));
    run(&again, PageOp::Rotate { pages: vec![1], degrees: 90 }).unwrap();
}

/// A document with a form of `n` text fields named `n0`, `n1`, ...
fn form_document(n: usize) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let fields: String = (0..n).map(|i| format!("{} 0 R ", 100 + i)).collect();
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{fields}] /DR << /Font << /Helv 9 0 R >> >> >> >>"));
    b.obj(2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>");
    b.obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>");
    for i in 0..n {
        b.obj(100 + i as u32, &format!("<< /FT /Tx /T (n{i}) /V (mine) >>"));
    }
    b.finish_classic(100 + n as u32, "/Root 1 0 R")
}

/// A file to put in with `roots` text fields of the first level that have `names` different names (`n0` ... as in [`form_document`]) and
/// `fonts` fonts in the resources of its form.
fn crowded_source(roots: usize, names: usize, fonts: usize) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let fields: String = (0..roots).map(|i| format!("{} 0 R ", 100 + i)).collect();
    let dr: String = (0..fonts).map(|i| format!("/F{i} 9 0 R ")).collect();
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{fields}] /DR << /Font << {dr} >> >> >> >>"));
    b.obj(2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>");
    b.obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>");
    for i in 0..roots {
        b.obj(100 + i as u32, &format!("<< /FT /Tx /T (n{}) /V (theirs) >>", i % names));
    }
    b.finish_classic(100 + roots as u32, "/Root 1 0 R")
}

fn field_names(doc: &Document) -> Vec<String> {
    let acro = doc.resolve(doc.catalog().unwrap().get("AcroForm").unwrap()).unwrap();
    acro.as_dict().unwrap().get("Fields").and_then(Object::as_array).unwrap().iter().map(|f| match doc.resolve(f).unwrap().as_dict().unwrap().get("T") {
        Some(Object::String(s)) => String::from_utf8_lossy(&s.bytes).into_owned(),
        _ => String::new(),
    }).collect()
}

fn font_entries(doc: &Document) -> usize {
    let acro = doc.resolve(doc.catalog().unwrap().get("AcroForm").unwrap()).unwrap();
    let dr = doc.resolve(acro.as_dict().unwrap().get("DR").unwrap()).unwrap();
    doc.resolve(dr.as_dict().unwrap().get("Font").unwrap()).unwrap().as_dict().unwrap().len()
}

#[test]
fn a_form_with_very_many_clashing_fields_and_resources_is_put_in_without_taking_quadratic_time() {
    use std::time::{Duration, Instant};
    let doc = Document::from_bytes(form_document(3_000)).unwrap();
    // 12 000 fields on 3 000 names the document has too, and 100 000 fonts: the fonts are not merged (more than may be), the names are
    // found once for each name, and it all ends in a moment.
    let path = source_on_disk("crowded.pdf", &crowded_source(12_000, 3_000, 100_000));
    let started = Instant::now();
    run(&doc, insert(1, &path)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    let names = field_names(&doc);
    assert_eq!(names.len(), 15_000);
    assert_eq!(names.iter().filter(|n| *n == "n5_2").count(), 4, "the fields that had one name get the same new one");
    assert_eq!(names.iter().filter(|n| *n == "n5").count(), 1);
    assert_eq!(font_entries(&doc), 1, "the resources of the other form were left out");
    // Fewer fonts than that are merged, the document's own staying as they are.
    let doc = Document::from_bytes(form_document(10)).unwrap();
    let path = source_on_disk("fonts.pdf", &crowded_source(5, 5, 9_000));
    run(&doc, insert(1, &path)).unwrap();
    assert_eq!(font_entries(&doc), 9_001);
    // A form with more fields than may be put in is refused, and nothing is written.
    let doc = Document::from_bytes(flat(2)).unwrap();
    let path = source_on_disk("too-many-fields.pdf", &crowded_source(20_001, 20_001, 1));
    assert!(matches!(run(&doc, insert(0, &path)), Err(Error::Limit(_))));
    assert!(!doc.has_overlay());
}

#[test]
fn a_file_bigger_than_what_may_be_put_in_is_refused_at_what_is_read_of_it() {
    let doc = Document::from_bytes(flat(2)).unwrap();
    let pages = doc.pages().unwrap();
    // One byte more than the most (a sparse file, so that nothing is written): refused with the message of the room that is too small.
    let heavy = std::env::temp_dir().join(format!("qingpdf-pages-test-{}-heavy.pdf", std::process::id()));
    let file = std::fs::File::create(&heavy).unwrap();
    file.set_len(MAX_INSERT_FILE_BYTES + 1).unwrap();
    drop(file);
    let path = heavy.to_string_lossy().into_owned();
    let refused = apply_any(&doc, &pages, &Edit::Pages(Box::new(insert(0, &path))), NOW, &Work::new(), DEFAULT_ROOM);
    assert!(matches!(&refused, Err(Error::Limit(m)) if m == TOO_BIG), "{:?}", refused.map(|_| ()));
    let _ = std::fs::remove_file(&heavy);
    // The most a file may be is what the memory for unsaved changes is: the two limits are one.
    assert_eq!(MAX_INSERT_FILE_BYTES, DEFAULT_ROOM as u64);
    // A file that is not bigger than that but bigger than what the room has left of it is refused as well.
    let some = source_on_disk("room.pdf", &source_file("X", 40));
    let size = std::fs::metadata(&some).unwrap().len() as usize;
    let refused = apply_any(&doc, &pages, &Edit::Pages(Box::new(insert(0, &some))), NOW, &Work::new(), size - 1);
    assert!(matches!(&refused, Err(Error::Limit(m)) if m == TOO_BIG), "{:?}", refused.map(|_| ()));
    assert!(!doc.has_overlay());
}

#[test]
fn a_page_with_no_type_that_was_deleted_is_not_written_again_by_the_links_and_bookmarks_that_name_it() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R 11 0 R] /Count 2 >>");
    // Pages that do not say that they are (7.7.3.3 asks for the /Type, files leave it out); a link on the first and a bookmark lead to the
    // second.
    b.obj(10, "<< /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 30 0 R /Annots [6 0 R] >>");
    b.obj(11, "<< /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 31 0 R >>");
    b.obj(6, "<< /Type /Annot /Subtype /Link /Rect [0 0 20 20] /Dest [11 0 R /Fit] >>");
    b.obj(5, "<< /Type /Outlines /First 7 0 R /Last 7 0 R /Count 1 >>");
    b.obj(7, "<< /Title (Second) /Parent 5 0 R /Dest [11 0 R /Fit] >>");
    b.stream_obj(30, "", b"KEPT-PAGE-MARK");
    b.stream_obj(31, "", b"DELETED-PAGE-MARK");
    let doc = Document::from_bytes(b.finish_classic(32, "/Root 1 0 R")).unwrap();
    assert_eq!(doc.page_count().unwrap(), 2);
    run(&doc, PageOp::Delete { pages: vec![1] }).unwrap();
    let out = crate::ops::copy_all(&doc).unwrap().data;
    let has = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"KEPT-PAGE-MARK"), "the page that stays is written");
    assert!(!has(b"DELETED-PAGE-MARK"), "the page that was taken out is not");
    assert_eq!(Document::from_bytes(out).unwrap().page_count().unwrap(), 1);
}
