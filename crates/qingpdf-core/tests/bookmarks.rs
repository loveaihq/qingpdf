//! Editing the bookmarks through the engine's messages (2026-10-20): added, typed over, taken out, moved, made a level deeper or shallower,
//! set to a place, made from the headings or from a text; undone, redone and saved. Every saved file is checked the same ways: the bytes of
//! the original are an exact prefix, `qpdf --check` accepts it, a checker of our own walks the tree of `/Parent`, `/Prev`, `/Next`, `/First`,
//! `/Last` and `/Count` of the file the way the specification (12.3.3) says it is tied together, our own reader reads the titles, levels
//! and pages the model of the test says, and the other readers are asked by `tests/tools/bookmarks_check.py` (PDFium and PyMuPDF: `get_toc`),
//! which reads the `manifest.json` these tests leave in `tests/out/bookmarks`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::Viewer;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use qingpdf_core::bookmarks::{self, Place};
use qingpdf_core::object::{Dict, ObjRef, Object};
use qingpdf_core::security::{Method, Permissions, Protection};
use qingpdf_core::view::{EditResult, EditStatus, Event, OutlineItem, SaveStatus};
use qingpdf_core::{Document, edit, ops};

// --- the engine, with bookmark calls ---------------------------------------------------------------------------------------------

trait Calls {
    fn outline(&mut self) -> Vec<OutlineItem>;
    fn set(&mut self, items: Vec<OutlineItem>) -> EditResult;
    fn headings(&mut self, only: Option<u32>) -> (SaveStatus, String, Vec<OutlineItem>);
    fn undo(&mut self) -> EditResult;
    fn redo(&mut self) -> EditResult;
    fn save(&mut self, path: &Path) -> (SaveStatus, String);
    fn save_as(&mut self, path: &Path) -> (SaveStatus, String);
}

impl Calls for Viewer {
    fn outline(&mut self) -> Vec<OutlineItem> {
        let id = self.id();
        self.engine.outline(1, id);
        self.until(|e| match e {
            Event::Outline { id: i, items, .. } if i == id => Some(items),
            _ => None,
        })
    }

    fn set(&mut self, items: Vec<OutlineItem>) -> EditResult {
        let id = self.id();
        self.engine.set_bookmarks(1, id, items);
        self.edited(id)
    }

    fn headings(&mut self, only: Option<u32>) -> (SaveStatus, String, Vec<OutlineItem>) {
        let id = self.id();
        self.engine.headings(1, id, only);
        self.until(|e| match e {
            Event::Headings { id: i, status, message, items, .. } if i == id => Some((status, message, items)),
            _ => None,
        })
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
        self.edited(id)
    }

    fn redo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.redo(1, id);
        self.edited(id)
    }

    fn save(&mut self, path: &Path) -> (SaveStatus, String) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, .. } if i == id => Some((status, message)),
            _ => None,
        })
    }

    fn save_as(&mut self, path: &Path) -> (SaveStatus, String) {
        let id = self.id();
        self.engine.save_as(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, .. } if i == id => Some((status, message)),
            _ => None,
        })
    }
}

// --- files and the other readers ------------------------------------------------------------------------------------------------

fn out_dir() -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("bookmarks");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

static MANIFEST: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Say, for the other readers, what `output` must show: for each bookmark its level (from 1), title and page (from 1, 0 if it leads nowhere).
fn expect_in_manifest(label: &str, output: &Path, password: &str, items: &[OutlineItem]) {
    let rows: Vec<String> = items.iter().map(|i| format!("[{},{},{}]", i.depth + 1, json(&i.title), i.page.map_or(0, |p| p + 1))).collect();
    let line = format!("{{\"label\":{},\"output\":{},\"password\":{},\"expect\":[{}]}}", json(label), json(&output.to_string_lossy().replace('\\', "/")), json(password), rows.join(","));
    let mut all = MANIFEST.lock().unwrap();
    all.retain(|l| !l.starts_with(&format!("{{\"label\":{},", json(label))));
    all.push(line);
    std::fs::write(out_dir().join("manifest.json"), format!("[\n{}\n]\n", all.join(",\n"))).unwrap();
}

/// qpdf accepts `saved`, and complains of nothing `original` did not complain of.
fn qpdf_accepts(saved: &Path, original: &Path, password: &str) {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED the qpdf check: qpdf was not found");
        return;
    };
    let check = |p: &Path| if password.is_empty() { common::qpdf_check(&qpdf, p) } else { common::qpdf_check_pw(&qpdf, p, password) };
    let (verdict, before) = (check(saved), check(original));
    let rank = |code: i32| match code {
        0 => 0,
        3 => 1,
        _ => 2,
    };
    assert!(rank(verdict.code) <= rank(before.code), "{}: qpdf --check says {}:\n{}", saved.display(), verdict.code, verdict.text);
    let known = common::qpdf_complaints(&before.text, original);
    let new: Vec<String> = common::qpdf_complaints(&verdict.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
    assert!(new.is_empty(), "{}: qpdf warns of what the original did not: {new:?}\n{}", saved.display(), verdict.text);
}

/// The title, level, page and open state of each bookmark: what is compared.
fn shape(items: &[OutlineItem]) -> Vec<(String, u32, Option<u32>, bool)> {
    // (An item shows its children open only if it has some.)
    items.iter().enumerate().map(|(k, i)| (i.title.clone(), i.depth, i.page, i.open && items.get(k + 1).is_some_and(|next| next.depth > i.depth))).collect()
}

/// A classic file from numbered objects (bodies as written between `obj` and `endobj`).
fn classic(objects: &[(u32, String)]) -> Vec<u8> {
    let mut bytes: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, bytes.len());
        bytes.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|x| x.0).max().unwrap() + 1;
    let at = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(off) => bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    bytes
}

/// How many objects the update that was added to `original` in `saved` writes.
fn objects_written(original: &Path, saved: &Path) -> usize {
    let (before, after) = (std::fs::read(original).unwrap(), std::fs::read(saved).unwrap());
    let added = &after[before.len()..];
    added.windows(6).filter(|w| *w == b" 0 obj").count()
}

/// The tie of the tree, read from the objects of the file the way 12.3.3 says it is made. Gives how many items it has.
fn verify_tree(doc: &Document) -> usize {
    let dict_of = |r: ObjRef| -> Dict {
        match doc.get(r).unwrap() {
            Object::Dict(d) => d,
            other => panic!("{r:?} is not a dictionary: {other:?}"),
        }
    };
    let cat = doc.catalog().unwrap();
    let Some(Object::Ref(root_ref)) = cat.get("Outlines").cloned() else { return 0 };
    let root = dict_of(root_ref);
    let r_of = |d: &Dict, key: &str| d.get(key).and_then(Object::as_obj_ref);
    // Items of one level: from `first` along `/Next`, ending at `last`. Gives (items below and at this level, items shown at this level and below).
    fn level(doc: &Document, parent: ObjRef, first: Option<ObjRef>, last: Option<ObjRef>, seen: &mut Vec<ObjRef>) -> (usize, i64) {
        let dict_of = |r: ObjRef| -> Dict {
            match doc.get(r).unwrap() {
                Object::Dict(d) => d,
                other => panic!("{r:?} is not a dictionary: {other:?}"),
            }
        };
        let r_of = |d: &Dict, key: &str| d.get(key).and_then(Object::as_obj_ref);
        let (mut total, mut shown) = (0usize, 0i64);
        let (mut prev, mut cur) = (None, first);
        while let Some(r) = cur {
            assert!(!seen.contains(&r), "{r:?} is in the tree twice");
            seen.push(r);
            let d = dict_of(r);
            assert!(matches!(d.get("Title"), Some(Object::String(_))), "{r:?} has no title");
            assert_eq!(r_of(&d, "Parent"), Some(parent), "{r:?}: /Parent");
            assert_eq!(r_of(&d, "Prev"), prev, "{r:?}: /Prev");
            let (below, shown_below) = match (r_of(&d, "First"), r_of(&d, "Last")) {
                (Some(f), Some(l)) => level(doc, r, Some(f), Some(l), seen),
                (None, None) => (0, 0),
                other => panic!("{r:?}: /First and /Last {other:?}"),
            };
            let count = d.get("Count").and_then(Object::as_int).unwrap_or(0);
            if below == 0 {
                assert_eq!(count, 0, "{r:?}: a bookmark with no children has a /Count");
            } else {
                assert!(count != 0, "{r:?}: a bookmark with children has no /Count");
                assert_eq!(count.abs(), shown_below, "{r:?}: /Count {count} but {shown_below} are shown below it");
            }
            total += 1 + below;
            shown += 1 + if count > 0 { shown_below } else { 0 };
            prev = Some(r);
            cur = r_of(&d, "Next");
        }
        assert_eq!(prev, last, "the last of the chain under {parent:?} is not its /Last");
        (total, shown)
    }
    let mut seen = Vec::new();
    let (total, shown) = match (r_of(&root, "First"), r_of(&root, "Last")) {
        (Some(f), Some(l)) => level(doc, root_ref, Some(f), Some(l), &mut seen),
        (None, None) => (0, 0),
        other => panic!("the outline: /First and /Last {other:?}"),
    };
    assert_eq!(root.get("Count").and_then(Object::as_int).unwrap_or(0), shown, "the outline's /Count");
    total
}

/// The whole round for a saved file: the original is the beginning of it, one update was added, qpdf accepts it, the tree is tied right, our
/// reader reads the model's bookmarks, and the manifest tells the other readers.
fn settle(label: &str, saved: &Path, original: &Path, password: &str, model: &[OutlineItem]) {
    let (before, after) = (std::fs::read(original).unwrap(), std::fs::read(saved).unwrap());
    assert!(after.starts_with(&before) && after.len() > before.len(), "{label}: the original is not the beginning of the saved file");
    let count = |bytes: &[u8]| bytes.windows(9).filter(|w| *w == b"startxref").count();
    assert_eq!(count(&after), count(&before) + 1, "{label}: more than one update was written");
    qpdf_accepts(saved, original, password);
    let doc = Document::open_with_password(saved, password).unwrap();
    assert!(!doc.was_repaired(), "{label}: the saved file needed its table rebuilt");
    assert_eq!(verify_tree(&doc), model.len(), "{label}: items in the tree");
    let (read, _) = qingpdf_core::view::read_bookmarks(&doc).unwrap();
    assert_eq!(shape(&read), shape(model), "{label}: the saved file read again");
    expect_in_manifest(label, saved, password, model);
}

fn item(title: &str, depth: u32, page: u32) -> OutlineItem {
    OutlineItem::made(title, depth, Some(page), Some(0.0), Some(30.0))
}

// --- the cases ----------------------------------------------------------------------------------------------------------------------

/// A Chinese official document with bookmarks: add, type over, take out, move, deeper and shallower, set to a place; then undone, redone, saved.
#[test]
fn a_chinese_gongwen_with_bookmarks_is_edited_undone_redone_and_saved() {
    let original = common::public_file("zh/gongwen/gongwen-2025-service-mfg-plan.pdf");
    let mut v = Viewer::open(&original, "");
    let start = v.outline();
    assert!(start.len() >= 15 && start.iter().all(|i| i.id != 0 && !i.placed), "{} bookmarks", start.len());
    let mut model = start.clone();
    let mut steps = 0;
    let mut apply = |v: &mut Viewer, model: &mut Vec<OutlineItem>, label: &str| {
        let r = v.set(model.clone());
        assert_eq!(r.status, EditStatus::Done, "{label}: {}", r.message);
        assert!(r.dirty && r.can_undo && !r.can_redo && r.page == qingpdf_core::view::NO_PAGE, "{label}: {r:?}");
        assert!(r.pages.is_none());
        let read = v.outline();
        assert_eq!(shape(&read), shape(model), "{label}: the bookmarks the engine reads after it");
        assert!(read.iter().all(|i| i.id != 0 && !i.placed), "{label}: every bookmark has its object now");
        *model = read;
        steps += 1;
    };

    // Type over a title (Chinese, longer than the old one).
    model[1].title = "二、主要任务（改过的标题）".to_string();
    apply(&mut v, &mut model, "rename");
    // Add one after the first, which has no children: a sibling.
    let at = bookmarks::add(&mut model, Some(0), item("新增书签", 7, 2)).unwrap();
    assert_eq!(at, 1);
    apply(&mut v, &mut model, "add");
    // Add at the end.
    bookmarks::add(&mut model, None, item("最后一个 last", 0, 8)).unwrap();
    apply(&mut v, &mut model, "add at the end");
    // Take out a branch (the second top-level item has children).
    let branch = (0..model.len()).find(|&i| model[i].depth == 0 && bookmarks::branch_end(&model, i) > i + 1).expect("an item with children");
    let gone = bookmarks::remove(&mut model, branch);
    assert!(gone > 1);
    apply(&mut v, &mut model, "delete a branch");
    // One level deeper, one shallower.
    let last_top = (0..model.len()).rev().find(|&i| model[i].depth == 0 && i > 0).unwrap();
    assert!(bookmarks::indent(&mut model, last_top));
    apply(&mut v, &mut model, "indent");
    assert!(bookmarks::outdent(&mut model, last_top));
    apply(&mut v, &mut model, "outdent");
    // Move: the last item inside the first; the first after the last (a whole branch, never into itself).
    let last = model.len() - 1;
    assert!(bookmarks::move_branch(&mut model, last, 0, Place::Inside).is_some());
    apply(&mut v, &mut model, "move inside");
    assert!(bookmarks::move_branch(&mut model, 0, 0, Place::After).is_none());
    let end = model.len() - 1;
    let to = bookmarks::move_branch(&mut model, 0, end, Place::After).unwrap();
    assert!(to > 0);
    apply(&mut v, &mut model, "move after");
    // Set one to a place of another page, and close an item.
    model[2].page = Some(5);
    model[2].x = Some(10.0);
    model[2].y = Some(120.0);
    model[2].placed = true;
    model[0].open = !model[0].open;
    apply(&mut v, &mut model, "set to a place");
    assert_eq!((model[2].page, model[2].y), (Some(5), Some(120.0)), "the place is read back");

    // Everything undone: the file saved is the file it was, byte for byte. Everything done again: the bookmarks are the model's.
    let mut undone = 0;
    loop {
        let r = v.undo();
        if r.status == EditStatus::Nothing {
            break;
        }
        assert_eq!((r.status, r.page), (EditStatus::Done, qingpdf_core::view::NO_PAGE), "{}", r.message);
        undone += 1;
    }
    assert_eq!(undone, steps);
    assert_eq!(shape(&v.outline()), shape(&start));
    let unchanged = out_dir().join("gongwen-undone.pdf");
    let (status, message) = v.save(&unchanged);
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert!(std::fs::read(&unchanged).unwrap() == std::fs::read(&original).unwrap(), "the file with every change undone is not the file");
    for _ in 0..steps {
        assert_eq!(v.redo().status, EditStatus::Done);
    }
    assert_eq!(v.redo().status, EditStatus::Nothing);
    assert_eq!(shape(&v.outline()), shape(&model));

    let saved = out_dir().join("gongwen.pdf");
    let (status, message) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    settle("gongwen", &saved, &original, "", &model);
    // Saved as a whole: the same bookmarks.
    let whole = out_dir().join("gongwen-whole.pdf");
    let (status, message) = Viewer::open(&saved, "").save_as(&whole);
    assert_eq!(status, SaveStatus::Done, "{message}");
    let doc = Document::open(&whole).unwrap();
    assert_eq!(shape(&qingpdf_core::view::read_bookmarks(&doc).unwrap().0), shape(&model), "save as");
    verify_tree(&doc);
    expect_in_manifest("gongwen-whole", &whole, "", &model);
    qpdf_accepts(&whole, &saved, "");
    // And then edited again after the save: the objects are the file's now.
    let more = v.outline();
    assert_eq!(shape(&more), shape(&model));
}

/// A file with no bookmarks is given them from its headings (and from one page's), at most three levels; the first one makes the outline.
#[test]
fn a_file_without_bookmarks_gets_them_from_the_headings_and_from_one_page() {
    let original = common::public_file("zh/gongwen/gongwen-2024-nicheng-work-report.pdf");
    let mut v = Viewer::open(&original, "");
    assert!(v.outline().is_empty());
    let (status, message, found) = v.headings(None);
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert!(found.len() >= 3 && found.iter().all(|i| i.depth < 3 && i.id == 0 && i.placed && i.page.is_some()), "{found:?}");
    bookmarks::check(&found).unwrap();
    // The headings of one page lead to it, whatever the levels are.
    let (_, _, one) = v.headings(Some(0));
    assert!(one.iter().all(|i| i.page == Some(0)), "{one:?}");
    // A bookmark made by hand first (Ctrl+B on a file with none) ...
    let r = v.set(vec![item("手工", 0, 1)]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert_eq!(shape(&v.outline()), vec![("手工".to_string(), 0, Some(1), false)]);
    // ... and then the headings replace it.
    let r = v.set(found.clone());
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let model = v.outline();
    assert_eq!(shape(&model).iter().map(|s| (s.0.clone(), s.1, s.2)).collect::<Vec<_>>(), shape(&found).iter().map(|s| (s.0.clone(), s.1, s.2)).collect::<Vec<_>>());
    let saved = out_dir().join("auto.pdf");
    let (status, message) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    settle("auto", &saved, &original, "", &model);
    // The outline made on a file that had none shows in the catalog.
    let doc = Document::open(&saved).unwrap();
    assert!(doc.catalog().unwrap().get("Outlines").is_some());
}

/// The 468-page book: the headings of the whole of it in under two seconds.
#[test]
fn the_headings_of_a_book_of_468_pages_are_found_within_two_seconds() {
    let original = common::corpus_root().join("local/zh/ebook/ebook-wikisource-yijikao-468p.pdf");
    if !original.is_file() {
        println!("SKIPPED: {} is not here", original.display());
        return;
    }
    let mut v = Viewer::open(&original, "");
    assert_eq!(v.opened.pages.len(), 468);
    // (The best of three: the machine this runs on may be busy with something else for a moment.)
    let mut best = std::time::Duration::MAX;
    let mut found = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let (status, message, items) = v.headings(None);
        let took = started.elapsed();
        assert_eq!(status, SaveStatus::Done, "{message}");
        found = items;
        best = best.min(took);
        if best.as_secs_f64() <= 2.0 {
            break;
        }
    }
    println!("headings of 468 pages: {} found in {best:?}", found.len());
    assert!(found.len() >= 20, "{} headings", found.len());
    assert!(best.as_secs_f64() <= 2.0, "{best:?}");
    let r = v.set(found);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let model = v.outline();
    let saved = out_dir().join("book-auto.pdf");
    let (status, message) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    settle("book-auto", &saved, &original, "", &model);
}

fn aes256(source: &Path, user: &str, owner: &str, permissions: Permissions, to: &Path) {
    let doc = Document::open(source).unwrap();
    let how = Protection { method: Method::AesV3, user_password: user.to_string(), owner_password: owner.to_string(), permissions, encrypt_metadata: true };
    let out = ops::protect(&doc, "", &how, &[7u8; 32]).unwrap();
    std::fs::write(to, out.data).unwrap();
}

/// An AES-256 file is edited under its own key and saved; its titles are encrypted in the update, qpdf reads them with the password.
#[test]
fn an_aes_256_file_is_edited_under_its_key_and_the_rights_decide() {
    let source = common::public_file("zh/gongwen/gongwen-2025-service-mfg-plan.pdf");
    // Everything allowed.
    let original = out_dir().join("aes-original.pdf");
    aes256(&source, "user pw", "owner pw", Permissions::all(), &original);
    let mut v = Viewer::open(&original, "user pw");
    let mut model = v.outline();
    assert!(model.len() >= 15);
    model[0].title = "加密文件里的新标题 new title".to_string();
    bookmarks::add(&mut model, Some(1), item("新书签", 0, 3)).unwrap();
    let r = v.set(model.clone());
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let model = v.outline();
    let saved = out_dir().join("aes.pdf");
    let (status, message) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    settle("aes", &saved, &original, "user pw", &model);
    // Saved as a whole: the same bookmarks, under the same key.
    let whole = out_dir().join("aes-whole.pdf");
    let (status, message) = Viewer::open(&saved, "user pw").save_as(&whole);
    assert_eq!(status, SaveStatus::Done, "{message}");
    let doc = Document::open_with_password(&whole, "user pw").unwrap();
    assert_eq!(shape(&qingpdf_core::view::read_bookmarks(&doc).unwrap().0), shape(&model), "save as, encrypted");
    verify_tree(&doc);
    qpdf_accepts(&whole, &saved, "user pw");
    expect_in_manifest("aes-whole", &whole, "user pw", &model);
    // The title is not in the file as text (the strings of an encrypted file are encrypted).
    assert!(!common::contains(&std::fs::read(&saved).unwrap()[std::fs::read(&original).unwrap().len()..], "new title".as_bytes()));

    // Bit 11 off, bit 4 on: allowed. Both off: denied, unless it was opened with the owner password.
    let only_modify = Permissions { assemble: false, ..Permissions::all() };
    let neither = Permissions { assemble: false, modify: false, ..Permissions::all() };
    let a = out_dir().join("aes-modify-only.pdf");
    aes256(&source, "", "owner pw", only_modify, &a);
    let mut v = Viewer::open(&a, "");
    let mut items = v.outline();
    items[0].title = "x".to_string();
    assert_eq!(v.set(items).status, EditStatus::Done);
    let b = out_dir().join("aes-neither.pdf");
    aes256(&source, "", "owner pw", neither, &b);
    let mut v = Viewer::open(&b, "");
    let mut items = v.outline();
    items[0].title = "x".to_string();
    let r = v.set(items.clone());
    assert_eq!(r.status, EditStatus::Denied, "{}", r.message);
    assert!(!r.dirty);
    let mut owner = Viewer::open(&b, "owner pw");
    assert_eq!(owner.set(items).status, EditStatus::Done);
}

/// A file with a signature is refused, with the words the pages are refused with.
#[test]
fn a_signed_file_is_refused() {
    let objects = [
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /AcroForm << /Fields [8 0 R] /SigFlags 3 >> >>".to_string()),
        (2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>".to_string()),
        (5, "<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>".to_string()),
        (6, "<< /Title (A) /Parent 5 0 R /Dest [10 0 R /Fit] >>".to_string()),
        (8, "<< /FT /Sig /T (Sig1) /V 20 0 R /Rect [0 0 100 30] /Subtype /Widget /Type /Annot /P 10 0 R >>".to_string()),
        (20, "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /Contents <00> /ByteRange [0 0 0 0] >>".to_string()),
        (10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Annots [8 0 R] >>".to_string()),
    ];
    let path = out_dir().join("signed.pdf");
    std::fs::write(&path, classic(&objects)).unwrap();
    let mut v = Viewer::open(&path, "");
    let mut items = v.outline();
    items[0].title = "B".to_string();
    let r = v.set(items);
    assert_eq!(r.status, EditStatus::Failed);
    assert!(r.message.contains("signed"), "{}", r.message);
    assert!(!r.dirty);
}

/// The bookmarks of a file are objects like any other: a bookmark that does something else than go to a place keeps what it does through every
/// change to its title and its place in the tree; named destinations are not touched.
#[test]
fn actions_other_than_go_to_stay_as_they_are() {
    let mut o: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /Names << /Dests 6 0 R >> >>".to_string()),
        (2, "<< /Type /Pages /Kids [10 0 R 12 0 R 14 0 R] /Count 3 >>".to_string()),
        (5, "<< /Type /Outlines /First 50 0 R /Last 56 0 R /Count 7 >>".to_string()),
        (6, "<< /Names [(here) [12 0 R /Fit] (there) [14 0 R /FitH 20]] >>".to_string()),
    ];
    for i in 0..3u32 {
        o.push((10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {} 0 R >>", 11 + 2 * i)));
        o.push((11 + 2 * i, "<< /Length 0 >>\nstream\n\nendstream".to_string()));
    }
    let items = [
        "/Dest [10 0 R /Fit] /C [1 0 0] /F 2",
        "/A << /S /URI /URI (https://example.com/a) >>",
        "/A << /S /Named /N /NextPage >>",
        "/A << /S /GoToR /F (other.pdf) /D [0 /Fit] >>",
        "/A << /S /JavaScript /JS (app.alert\\(1\\)) >>",
        "/Dest (here)",
        "/A << /S /GoTo /D (there) >>",
    ];
    for (k, extra) in items.iter().enumerate() {
        let n = 50 + k as u32;
        let prev = if k > 0 { format!(" /Prev {} 0 R", n - 1) } else { String::new() };
        let next = if k + 1 < items.len() { format!(" /Next {} 0 R", n + 1) } else { String::new() };
        o.push((n, format!("<< /Title (Item {k}) /Parent 5 0 R {extra}{prev}{next} >>")));
    }
    let original = out_dir().join("actions-original.pdf");
    std::fs::write(&original, classic(&o)).unwrap();

    let before = Document::open(&original).unwrap();
    let kept = |doc: &Document, num: u32| -> (Option<Object>, Option<Object>, Option<Object>) {
        let Object::Dict(d) = doc.get(ObjRef::new(num, 0)).unwrap() else { panic!("not a dictionary") };
        (d.get("A").cloned(), d.get("Dest").cloned(), d.get("C").cloned())
    };
    let mut v = Viewer::open(&original, "");
    let mut model = v.outline();
    assert_eq!(model.iter().map(|i| i.action).collect::<Vec<_>>(), vec![false, true, true, true, true, false, false]);
    assert_eq!(model.iter().map(|i| i.page).collect::<Vec<_>>(), vec![Some(0), None, None, None, None, Some(1), Some(2)]);
    // Title changes, a move, a level, a delete: the actions stay.
    model[1].title = "Link renamed 链接".to_string();
    model[3].title = "Remote renamed".to_string();
    assert!(bookmarks::indent(&mut model, 2));
    assert!(bookmarks::move_branch(&mut model, 4, 0, Place::Before).is_some());
    let gone = model.remove(5);
    assert_eq!(gone.title, "Item 5");
    let r = v.set(model.clone());
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let model = v.outline();
    let saved = out_dir().join("actions.pdf");
    let (status, message) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    settle("actions", &saved, &original, "", &model);
    let after = Document::open(&saved).unwrap();
    for n in 50..57u32 {
        if n == 55 {
            continue;
        }
        assert_eq!(kept(&before, n), kept(&after, n), "object {n}: its action, destination and colour");
    }
    // The named destinations are as they were.
    assert_eq!(before.get(ObjRef::new(6, 0)).unwrap(), after.get(ObjRef::new(6, 0)).unwrap());
    assert!(objects_written(&original, &saved) <= 7, "{} objects", objects_written(&original, &saved));

    // Setting the place of an item with a name replaces the name; an item with an action is not given a place (the engine keeps its action).
    let mut v = Viewer::open(&saved, "");
    let mut items = v.outline();
    let named = items.iter().position(|i| i.title == "Item 5").is_none();
    assert!(named);
    let link = items.iter().position(|i| i.title == "Link renamed 链接").unwrap();
    items[link].page = Some(1);
    items[link].placed = true;
    items[link].y = Some(10.0);
    let r = v.set(items);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let after_set = v.outline();
    let again = after_set.iter().find(|i| i.title == "Link renamed 链接").unwrap();
    assert!(again.action && again.page.is_none(), "{again:?}: the action of a link is not replaced by a place");
}

/// Export and import: the text of the bookmarks makes the same bookmarks in a file that has none.
#[test]
fn the_text_of_the_bookmarks_makes_the_same_bookmarks() {
    let original = common::public_file("zh/gongwen/gongwen-2025-service-mfg-plan.pdf");
    let doc = Document::open(&original).unwrap();
    let (items, _) = qingpdf_core::view::read_bookmarks(&doc).unwrap();
    let text = bookmarks::export_text(&items);
    let pages = doc.page_count().unwrap();
    let back = bookmarks::parse_text(&text, pages).unwrap();
    assert_eq!(back.iter().map(|i| (i.title.clone(), i.depth, i.page)).collect::<Vec<_>>(), items.iter().map(|i| (i.title.clone(), i.depth, i.page)).collect::<Vec<_>>());
    let made = edit::set_outline(&doc, "", back).unwrap();
    let saved = out_dir().join("imported.pdf");
    std::fs::write(&saved, &made.data).unwrap();
    let again = Document::open(&saved).unwrap();
    verify_tree(&again);
    let (read, _) = qingpdf_core::view::read_bookmarks(&again).unwrap();
    assert_eq!(bookmarks::export_text(&read), text, "export, import, export");
    qpdf_accepts(&saved, &original, "");
    expect_in_manifest("imported", &saved, "", &read);
    // A file with a different number of pages refuses a page that is not in it.
    assert!(bookmarks::parse_text(&text, 2).unwrap_err().contains("line"));
}

/// A synthetic file with `n` bookmarks, three levels, over 50 pages.
fn big_outline(n: usize) -> Vec<u8> {
    let mut o: Vec<(u32, String)> = vec![(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>".to_string())];
    let kids: String = (0..50).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    o.push((2, format!("<< /Type /Pages /Kids [{kids}] /Count 50 >>")));
    for i in 0..50u32 {
        o.push((10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {} 0 R >>", 11 + 2 * i)));
        o.push((11 + 2 * i, "<< /Length 0 >>\nstream\n\nendstream".to_string()));
    }
    // 1 chapter in 25 is a top-level item with sections below it (1 in 5 of them with subsections).
    let depth = |i: usize| if i.is_multiple_of(25) { 0 } else if i.is_multiple_of(5) { 2 } else { 1 };
    let items: Vec<OutlineItem> = (0..n).map(|i| item(&format!("标题 {i}"), depth(i), (i % 50) as u32)).collect();
    let (counts, root_count) = bookmarks::counts(&items.iter().cloned().map(|mut i| { i.open = true; i }).collect::<Vec<_>>());
    // For each: its parent, the one before, the one after, its first child and its last.
    type Links = (Option<usize>, Option<usize>, Option<usize>, Option<usize>, Option<usize>);
    let mut links: Vec<Links> = vec![(None, None, None, None, None); n];
    let (mut top_first, mut top_last) = (None, None);
    let mut chain: Vec<usize> = Vec::new();
    let mut last_child: Vec<Option<usize>> = vec![None; n];
    for (i, it) in items.iter().enumerate() {
        chain.truncate(it.depth as usize);
        let parent = chain.last().copied();
        let prev = match parent {
            Some(p) => last_child[p],
            None => top_last,
        };
        if let Some(pr) = prev {
            links[pr].2 = Some(i);
        }
        links[i].0 = parent;
        links[i].1 = prev;
        match parent {
            Some(p) => {
                if links[p].3.is_none() {
                    links[p].3 = Some(i);
                }
                links[p].4 = Some(i);
                last_child[p] = Some(i);
            }
            None => {
                top_first = top_first.or(Some(i));
                top_last = Some(i);
            }
        }
        chain.push(i);
    }
    let num = |i: usize| 1000 + i as u32;
    o.push((5, format!("<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {root_count} >>", num(top_first.unwrap()), num(top_last.unwrap()))));
    for (i, it) in items.iter().enumerate() {
        let (parent, prev, next, first, last) = links[i];
        let mut s = format!("<< /Title ({}) /Parent {} 0 R", ascii_title(i), parent.map_or(5, num));
        for (key, v) in [("Prev", prev), ("Next", next), ("First", first), ("Last", last)] {
            if let Some(v) = v {
                s.push_str(&format!(" /{key} {} 0 R", num(v)));
            }
        }
        if counts[i] != 0 {
            s.push_str(&format!(" /Count {}", counts[i]));
        }
        s.push_str(&format!(" /Dest [{} 0 R /XYZ 0 90 0] >>", 10 + 2 * it.page.unwrap()));
        o.push((num(i), s));
    }
    classic(&o)
}

fn ascii_title(i: usize) -> String {
    format!("Item {i}")
}

/// 5,000 bookmarks: an edit that rewrites all of them, and one that changes a few, each with its save, in 300 ms.
#[test]
fn an_outline_of_five_thousand_items_is_edited_and_saved_within_300_milliseconds() {
    let original = out_dir().join("big-original.pdf");
    std::fs::write(&original, big_outline(5000)).unwrap();
    let mut v = Viewer::open(&original, "");
    let started = Instant::now();
    let items = v.outline();
    println!("read 5000 bookmarks: {:?}", started.elapsed());
    assert_eq!(items.len(), 5000);
    verify_tree(&Document::open(&original).unwrap());

    // A few changes: a title, a move of a whole branch, a deletion.
    let mut model = items.clone();
    model[2500].title = "改名".to_string();
    let to = bookmarks::move_branch(&mut model, 25, 4000, Place::After).unwrap();
    assert!(to > 3000);
    bookmarks::remove(&mut model, 100);
    let started = Instant::now();
    let r = v.set(model.clone());
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let saved = out_dir().join("big.pdf");
    let (status, message) = v.save(&saved);
    let few = started.elapsed();
    assert_eq!(status, SaveStatus::Done, "{message}");
    println!("a few changes to 5000 bookmarks, edit and save: {few:?}");
    assert!(few.as_millis() <= 300, "{few:?}");
    let model = v.outline();
    settle("big", &saved, &original, "", &model);

    // All of them rewritten: every title different (the list from the text of the bookmarks, all new objects).
    let mut all = model.clone();
    for (i, it) in all.iter_mut().enumerate() {
        it.title = format!("全部重写 {i}");
    }
    let started = Instant::now();
    let r = v.set(all.clone());
    let edited = started.elapsed();
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let again = out_dir().join("big-all.pdf");
    let (status, message) = v.save(&again);
    let everything = started.elapsed();
    assert_eq!(status, SaveStatus::Done, "{message}");
    println!("every one of 5000 bookmarks changed, edit and save: {everything:?} (the edit {edited:?})");
    assert!(everything.as_millis() <= 300, "{everything:?}");
    let model = v.outline();
    settle("big-all", &again, &saved, "", &model);

    // From the text: 5000 new objects.
    let text = bookmarks::export_text(&model);
    let fresh = bookmarks::parse_text(&text, 50).unwrap();
    assert_eq!(fresh.len(), 4975);
    let started = Instant::now();
    let r = v.set(fresh);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let imported = out_dir().join("big-import.pdf");
    let (status, message) = v.save(&imported);
    let replaced = started.elapsed();
    assert_eq!(status, SaveStatus::Done, "{message}");
    println!("5000 new bookmarks, edit and save: {replaced:?}");
    assert!(replaced.as_millis() <= 300, "{replaced:?}");
    let model = v.outline();
    settle("big-import", &imported, &again, "", &model);
}

/// A list that is not the file's (a bookmark that names an object that is not one of its bookmarks, or one twice, a level that jumps, too many
/// levels, a page that is not there) is refused and changes nothing.
#[test]
fn a_list_that_is_not_the_files_is_refused() {
    let original = common::public_file("zh/gongwen/gongwen-2025-service-mfg-plan.pdf");
    let mut v = Viewer::open(&original, "");
    let good = v.outline();
    let refused = |v: &mut Viewer, items: Vec<OutlineItem>, why: &str| {
        let r = v.set(items);
        assert_eq!(r.status, EditStatus::Failed, "{why}");
        assert!(!r.dirty && !r.can_undo, "{why}: {r:?}");
    };
    let mut bad = good.clone();
    bad[1].id = 4_000_000;
    refused(&mut v, bad, "an object that is not a bookmark");
    let mut bad = good.clone();
    bad[1].id = bad[0].id;
    refused(&mut v, bad, "one bookmark twice");
    let mut bad = good.clone();
    bad[1].depth = 5;
    refused(&mut v, bad, "a level that jumps");
    let mut bad = good.clone();
    bad[0].depth = 1;
    refused(&mut v, bad, "the first not at the top");
    let mut bad = good.clone();
    bad[0].title = "x".repeat(500);
    refused(&mut v, bad, "a title that is too long");
    let mut bad = good.clone();
    bad[0].page = Some(900);
    bad[0].placed = true;
    refused(&mut v, bad, "a page that is not there");
    let mut bad = good.clone();
    bad[0].y = Some(f32::NAN);
    bad[0].placed = true;
    refused(&mut v, bad, "a place that is not a number");
    assert_eq!(shape(&v.outline()), shape(&good));
    // Headings of a page that is not there.
    let (status, ..) = v.headings(Some(900));
    assert_eq!(status, SaveStatus::Failed);
}

// --- the fixes of the review (2026-10-20) ------------------------------------------------------------------------------------------

/// The same file with its cross-reference table cut out: the number after `startxref` leads to nothing, and the file is read by scanning.
fn damaged(bytes: &[u8]) -> Vec<u8> {
    let find = |needle: &[u8]| bytes.windows(needle.len()).rposition(|w| w == needle).unwrap();
    let (table, trailer) = (find(b"\nxref\n") + 1, find(b"trailer"));
    let mut out = bytes[..table].to_vec();
    out.extend_from_slice(&bytes[trailer..]);
    out
}

fn ids(items: &[OutlineItem]) -> Vec<u32> {
    items.iter().map(|i| i.id).collect()
}

/// The list as the engine says it, and whether it was cut short.
fn outline_cut(v: &mut Viewer) -> (Vec<OutlineItem>, bool) {
    let id = v.id();
    v.engine.outline(1, id);
    v.until(|e| match e {
        Event::Outline { id: i, items, truncated, .. } if i == id => Some((items, truncated)),
        _ => None,
    })
}

/// A damaged file is written afresh before its first change, and that numbers its objects anew: a change to the bookmarks that names the old
/// numbers is still the change it was.
#[test]
fn the_first_change_to_the_bookmarks_of_a_damaged_file_is_made_with_the_new_numbers() {
    let input = out_dir().join("damaged-outline.pdf");
    std::fs::write(&input, damaged(&big_outline(60))).unwrap();
    assert!(Document::open(&input).unwrap().was_repaired(), "the test file is not damaged");
    let mut v = Viewer::open(&input, "");
    let mut model = v.outline();
    assert_eq!(model.len(), 60);
    let was = ids(&model);
    model[7].title = "Renamed after the rewrite".to_string();
    bookmarks::remove(&mut model, 30);
    bookmarks::add(&mut model, None, item("Added", 0, 3)).unwrap();
    let r = v.set(model.clone());
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(r.rewrote, "the file was not written afresh");
    let after = v.outline();
    assert_eq!(shape(&after), shape(&model));
    assert_ne!(ids(&after)[..7], was[..7], "the objects were not numbered anew, so the test shows nothing");
    // The next change is made with the list as the engine now says it.
    let mut again = after.clone();
    again[0].title = "Second change".to_string();
    assert_eq!(v.set(again.clone()).status, EditStatus::Done);
    let saved = out_dir().join("damaged-outline-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let doc = Document::open(&saved).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(verify_tree(&doc), again.len());
    assert_eq!(shape(&qingpdf_core::view::read_bookmarks(&doc).unwrap().0), shape(&again));
    qpdf_accepts(&saved, &input, "");
    expect_in_manifest("damaged-outline", &saved, "", &again);
    // The original is untouched.
    assert_eq!(std::fs::read(&input).unwrap(), damaged(&big_outline(60)));
}

/// A change that waited for the rewrite names its items by position as well: an item the old file has not is left as it is, and the change
/// is then refused, not applied to some other item.
#[test]
fn a_change_that_waited_for_the_rewrite_does_not_land_on_another_item() {
    let input = out_dir().join("damaged-outline-wrong.pdf");
    std::fs::write(&input, damaged(&big_outline(30))).unwrap();
    let mut v = Viewer::open(&input, "");
    let mut model = v.outline();
    model[3].id = 4_000_000;
    let r = v.set(model);
    assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    assert_eq!(v.outline().len(), 30);
}

/// Save as writes the whole file again, and the objects of the file are numbered anew: the list read before names numbers that are not in the
/// file any more, and the list read after is the one to change.
#[test]
fn after_a_save_as_the_bookmarks_are_read_again_with_the_numbers_of_the_new_file() {
    let original = out_dir().join("save-as-original.pdf");
    std::fs::write(&original, big_outline(40)).unwrap();
    let mut v = Viewer::open(&original, "");
    let before = v.outline();
    let copy = out_dir().join("save-as-copy.pdf");
    assert_eq!(v.save_as(&copy).0, SaveStatus::Done);
    let after = v.outline();
    assert_eq!(shape(&after), shape(&before));
    assert_ne!(ids(&after), ids(&before), "save as did not number the objects anew, so the test shows nothing");
    let mut stale = before.clone();
    stale[3].title = "from the list before".to_string();
    let r = v.set(stale);
    assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    let mut fresh = after.clone();
    fresh[3].title = "from the list after".to_string();
    assert_eq!(v.set(fresh.clone()).status, EditStatus::Done);
    let saved = out_dir().join("save-as-edited.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    settle("save-as-edited", &saved, &copy, "", &fresh);
}

/// A file whose bookmarks lead to a page, to the node of the page tree, to the catalog or to something that is no bookmark has them left out:
/// they are not shown, and never written as bookmarks (a page would have lost its inherited size).
#[test]
fn a_chain_of_bookmarks_that_leads_to_pages_the_catalog_or_the_outline_does_not_rewrite_them() {
    let objects: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /Next 5 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [10 0 R 12 0 R] /Count 2 /MediaBox [0 0 200 100] /Next 30 0 R >>".to_string()),
        (5, "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 2 >>".to_string()),
        (10, "<< /Type /Page /Parent 2 0 R /Contents 11 0 R /Next 12 0 R /First 12 0 R >>".to_string()),
        (11, "<< /Length 0 >>\nstream\n\nendstream".to_string()),
        (12, "<< /Type /Page /Parent 2 0 R /Contents 13 0 R /Next 2 0 R >>".to_string()),
        (13, "<< /Length 0 >>\nstream\n\nendstream".to_string()),
        (20, "<< /Title (Real) /Parent 5 0 R /Next 10 0 R /First 21 0 R /Last 21 0 R /Count 1 /Dest [10 0 R /Fit] >>".to_string()),
        (21, "<< /Title (Child) /Parent 20 0 R /Next 1 0 R /Dest [12 0 R /Fit] >>".to_string()),
        // (No type: a node of the page tree is known by its /Kids.)
        (30, "<< /Kids [10 0 R] /Next 1 0 R /Parent 5 0 R >>".to_string()),
    ];
    let original = out_dir().join("hostile-outline.pdf");
    std::fs::write(&original, classic(&objects)).unwrap();
    let doc = Document::open(&original).unwrap();
    let (read, cut) = qingpdf_core::view::read_bookmarks(&doc).unwrap();
    assert!(!cut);
    assert_eq!(read.iter().map(|i| (i.title.as_str(), i.depth, i.id)).collect::<Vec<_>>(), vec![("Real", 0, 20), ("Child", 1, 21)]);
    let before: Vec<Object> = [1, 2, 10, 11, 12, 13, 30].iter().map(|&n| doc.get(ObjRef::new(n, 0)).unwrap()).collect();

    let mut v = Viewer::open(&original, "");
    let mut model = v.outline();
    assert_eq!(model.len(), 2);
    // A page, the node of the page tree, the catalog and the object without a type are not bookmarks to name.
    for n in [1, 2, 10, 30] {
        let mut bad = model.clone();
        bad[0].id = n;
        let r = v.set(bad);
        assert_eq!(r.status, EditStatus::Failed, "object {n}: {}", r.message);
    }
    model[0].title = "Renamed".to_string();
    bookmarks::add(&mut model, None, item("Added", 0, 1)).unwrap();
    let r = v.set(model.clone());
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let saved = out_dir().join("hostile-outline-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let after = Document::open(&saved).unwrap();
    for (k, n) in [1, 2, 10, 11, 12, 13, 30].iter().enumerate() {
        assert_eq!(after.get(ObjRef::new(*n, 0)).unwrap(), before[k], "object {n} was changed");
    }
    // (The tree of the outline is tied as the specification says, and the pages still have their size from the node above them.)
    assert_eq!(shape(&qingpdf_core::view::read_bookmarks(&after).unwrap().0), shape(&model));
    assert_eq!(after.pages().unwrap().len(), 2);
    qpdf_accepts(&saved, &original, "");
}

/// A file with more bookmarks than are read (too many, or nested too deep) is not edited, for the ones not read would be lost; a list that is
/// all new replaces them whole.
#[test]
fn bookmarks_beyond_the_limits_are_not_edited_but_a_new_list_replaces_them() {
    let too_many = out_dir().join("outline-20001.pdf");
    std::fs::write(&too_many, big_outline(qingpdf_core::view::MAX_OUTLINE_ITEMS + 1)).unwrap();
    // Nested one level deeper than is read: a chain of 40 bookmarks each inside the one before.
    let levels = qingpdf_core::view::MAX_OUTLINE_DEPTH + 8;
    let mut objects: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>".to_string()),
        (10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>".to_string()),
        (5, format!("<< /Type /Outlines /First 100 0 R /Last 100 0 R /Count {levels} >>")),
    ];
    for i in 0..levels {
        let parent = if i == 0 { 5 } else { 99 + i };
        let below = if i + 1 < levels { format!(" /First {} 0 R /Last {} 0 R /Count {}", 101 + i, 101 + i, levels - i - 1) } else { String::new() };
        objects.push((100 + i as u32, format!("<< /Title (Level {i}) /Parent {parent} 0 R{below} /Dest [10 0 R /Fit] >>")));
    }
    let too_deep = out_dir().join("outline-too-deep.pdf");
    std::fs::write(&too_deep, classic(&objects)).unwrap();

    for (file, what) in [(&too_many, "more than the most"), (&too_deep, "deeper than the most")] {
        let doc = Document::open(file).unwrap();
        let (read, cut) = qingpdf_core::view::read_bookmarks(&doc).unwrap();
        assert!(cut, "{what}: not said to be cut");
        assert!(read.len() <= qingpdf_core::view::MAX_OUTLINE_ITEMS, "{what}");
        let mut v = Viewer::open(file, "");
        let (model, cut) = outline_cut(&mut v);
        assert!(cut && !model.is_empty(), "{what}");
        // A change of what is read would lose the rest: refused, and nothing is changed.
        let mut renamed = model.clone();
        renamed[0].title = "Renamed".to_string();
        let r = v.set(renamed);
        assert_eq!(r.status, EditStatus::Failed, "{what}: {}", r.message);
        assert!(r.message.contains("more than can be edited"), "{what}: {}", r.message);
        assert!(!r.dirty && !r.can_undo, "{what}");
        // A list of new bookmarks only (an import, the headings) takes their place.
        let fresh = vec![item("First chapter", 0, 0), item("A section", 1, 0), item("Second chapter", 0, 0)];
        let r = v.set(fresh.clone());
        assert_eq!(r.status, EditStatus::Done, "{what}: {}", r.message);
        let saved = out_dir().join(format!("{}-replaced.pdf", file.file_stem().unwrap().to_string_lossy()));
        assert_eq!(v.save(&saved).0, SaveStatus::Done, "{what}");
        let again = Document::open(&saved).unwrap();
        let (read, cut) = qingpdf_core::view::read_bookmarks(&again).unwrap();
        assert!(!cut, "{what}: still cut after the replacement");
        assert_eq!(shape(&read), shape(&fresh), "{what}");
        assert_eq!(verify_tree(&again), 3, "{what}");
        qpdf_accepts(&saved, file, "");
        // The command line's way (no engine) is the same.
        let doc = Document::open(file).unwrap();
        let made = edit::set_outline(&doc, "", fresh.clone()).unwrap();
        let once_more = Document::from_bytes(made.data).unwrap();
        assert_eq!(shape(&qingpdf_core::view::read_bookmarks(&once_more).unwrap().0), shape(&fresh), "{what}: set_outline");
        let refused = edit::set_outline(&doc, "", model.clone());
        assert!(refused.is_err(), "{what}: a list with the file's own bookmarks was taken");
    }
}

/// Only the objects that change are written: a title is one object; a bookmark added at the end, the few it ties to.
#[test]
fn a_change_writes_only_the_objects_it_changes() {
    // (A file whose /Count are what 12.3.3 says: a file of another convention has them put right where the change reaches.)
    let original = out_dir().join("written-original.pdf");
    std::fs::write(&original, big_outline(200)).unwrap();
    let mut v = Viewer::open(&original, "");
    let mut model = v.outline();
    let last = model.len() - 1;
    model[last].title = "Last one renamed".to_string();
    assert_eq!(v.set(model.clone()).status, EditStatus::Done);
    let saved = out_dir().join("written-rename.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    assert_eq!(objects_written(&original, &saved), 1);

    let mut v = Viewer::open(&original, "");
    let mut model = v.outline();
    bookmarks::add(&mut model, None, item("At the end", 0, 4)).unwrap();
    assert_eq!(v.set(model.clone()).status, EditStatus::Done);
    let saved = out_dir().join("written-add.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    // The new object, the last top-level item before it (its /Next), the outline (/Last and /Count).
    assert_eq!(objects_written(&original, &saved), 3);
    // One taken out from among the others: its brothers on both sides, its parent (first, last, count) and the outline.
    let mut v = Viewer::open(&original, "");
    let mut model = v.outline();
    assert_eq!(model[7].depth, 1);
    bookmarks::remove(&mut model, 7);
    assert_eq!(v.set(model.clone()).status, EditStatus::Done);
    let saved = out_dir().join("written-delete.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    // (Items 6 and 8, the parent 0, and the outline.)
    assert!(objects_written(&original, &saved) <= 5, "{}", objects_written(&original, &saved));
    settle("written-delete", &saved, &original, "", &v.outline());
    let mut v = Viewer::open(&original, "");
    let mut model = v.outline();
    bookmarks::add(&mut model, None, item("At the end", 0, 4)).unwrap();
    assert_eq!(v.set(model.clone()).status, EditStatus::Done);
    let saved = out_dir().join("written-add.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    settle("written-add", &saved, &original, "", &v.outline());
}
