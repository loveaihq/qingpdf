//! Page management through the engine's messages (2026-10-15): pages turned, taken out, moved and put in from another file, undone and
//! redone, saved as the one update, saved as a whole, extracted. Every saved file is checked the same ways: the bytes of the original
//! are an exact prefix, `qpdf --check` accepts it, our own reader opens it again and sees the pages the model of the test says, and the
//! other readers are asked by `tests/tools/pages_check.py` (PDFium: the number of pages, their order, their turn), which reads the
//! `manifest.json` these tests leave in `tests/out/pages`. A file whose edits are all undone is saved as the file it was.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::Viewer;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use qingpdf_core::Document;
use qingpdf_core::view::{EditResult, EditStatus, Event, LinkKind, PageInfo, SaveStatus, order_after_move};

/// Where a page of the model comes from: the file that was opened, or the file pages were put in from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Src {
    Orig(usize),
    Ins(usize),
}

/// What the pages should be after the changes: for each, where it came from and how many degrees it was turned by.
#[derive(Clone, Debug)]
struct Model {
    pages: Vec<(Src, i32)>,
}

impl Model {
    fn new(n: usize) -> Model {
        Model { pages: (0..n).map(|i| (Src::Orig(i), 0)).collect() }
    }

    fn rotate(&mut self, which: &[u32], degrees: i32) {
        for &p in which {
            self.pages[p as usize].1 = (self.pages[p as usize].1 + degrees).rem_euclid(360);
        }
    }

    fn delete(&mut self, which: &[u32]) {
        let mut i = 0;
        self.pages.retain(|_| {
            i += 1;
            !which.contains(&(i - 1))
        });
    }

    fn mv(&mut self, which: &[u32], to: u32) {
        let order = order_after_move(self.pages.len(), which, to as usize);
        self.pages = order.iter().map(|&i| self.pages[i as usize]).collect();
    }

    fn insert(&mut self, at: usize, count: usize) {
        let new: Vec<(Src, i32)> = (0..count).map(|j| (Src::Ins(j), 0)).collect();
        self.pages.splice(at..at, new);
    }
}

/// The size of a page of `sizes` after it is turned by `turn` more degrees.
fn turned(size: PageInfo, turn: i32) -> (i64, i64) {
    let (w, h) = (size.width.round() as i64, size.height.round() as i64);
    if turn % 180 == 90 { (h, w) } else { (w, h) }
}

// --- the engine, with page calls ---------------------------------------------------------------------------------------------------

trait PageCalls {
    fn pages_done(&mut self, call: impl FnOnce(&qingpdf_core::view::Engine, u64)) -> EditResult;
    fn rotate(&mut self, pages: &[u32], degrees: i32) -> EditResult;
    fn delete(&mut self, pages: &[u32]) -> EditResult;
    fn mv(&mut self, pages: &[u32], to: u32) -> EditResult;
    fn insert(&mut self, at: u32, path: &Path, password: &str) -> EditResult;
    fn undo(&mut self) -> EditResult;
    fn redo(&mut self) -> EditResult;
    fn save(&mut self, path: &Path) -> (SaveStatus, String);
    fn save_as(&mut self, path: &Path) -> (SaveStatus, String);
    fn extract(&mut self, pages: &[u32], path: &Path) -> (SaveStatus, String, u32);
}

impl PageCalls for Viewer {
    fn pages_done(&mut self, call: impl FnOnce(&qingpdf_core::view::Engine, u64)) -> EditResult {
        let id = self.id();
        call(&self.engine, id);
        self.edited(id)
    }

    fn rotate(&mut self, pages: &[u32], degrees: i32) -> EditResult {
        self.pages_done(|e, id| e.rotate_pages(1, id, pages.to_vec(), degrees))
    }

    fn delete(&mut self, pages: &[u32]) -> EditResult {
        self.pages_done(|e, id| e.delete_pages(1, id, pages.to_vec()))
    }

    fn mv(&mut self, pages: &[u32], to: u32) -> EditResult {
        self.pages_done(|e, id| e.move_pages(1, id, pages.to_vec(), to))
    }

    fn insert(&mut self, at: u32, path: &Path, password: &str) -> EditResult {
        self.pages_done(|e, id| e.insert_pages(1, id, at, &path.to_string_lossy(), password))
    }

    fn undo(&mut self) -> EditResult {
        self.pages_done(|e, id| e.undo(1, id))
    }

    fn redo(&mut self) -> EditResult {
        self.pages_done(|e, id| e.redo(1, id))
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

    fn extract(&mut self, pages: &[u32], path: &Path) -> (SaveStatus, String, u32) {
        let id = self.id();
        self.engine.extract_pages(1, id, pages.to_vec(), &path.to_string_lossy());
        self.until(|e| match e {
            Event::Exported { id: i, status, message, pages, .. } if i == id => Some((status, message, pages)),
            _ => None,
        })
    }
}

// --- files ---------------------------------------------------------------------------------------------------------------------------

/// Write a file the tests share (they run side by side): once, not over again while another test reads it.
fn shared_file(path: &Path, bytes: &[u8]) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _guard = WRITING.lock().unwrap();
    if std::fs::read(path).ok().as_deref() != Some(bytes) {
        std::fs::write(path, bytes).unwrap();
    }
}

fn out_dir() -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("pages");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file of `n` pages in one node, page `i` with the words `{tag}-{i:04}` on it and a width of `200 + i % 50` points (so that the pages
/// can be told apart by what they say and by their size).
fn synthetic(n: usize, tag: &str) -> Vec<u8> {
    let mut objects: Vec<(u32, String)> = vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string())];
    let kids: String = (0..n).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    objects.push((2, format!("<< /Type /Pages /Kids [{kids}] /Count {n} /Resources << /Font << /F1 3 0 R >> >> >>")));
    objects.push((3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()));
    for i in 0..n {
        let (page, content) = (10 + 2 * i as u32, 11 + 2 * i as u32);
        objects.push((page, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 300] /Contents {content} 0 R >>", 200 + i % 50)));
        let body = format!("BT /F1 24 Tf 20 100 Td ({tag}-{i:04}) Tj ET");
        objects.push((content, format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())));
    }
    assemble(&objects, "/Root 1 0 R")
}

/// A classic file from numbered objects (bodies as written between `obj` and `endobj`).
fn assemble(objects: &[(u32, String)], trailer: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} {trailer} >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

/// Eight pages with bookmarks to each, links (by name and by explicit destination) between them, text fields on pages 1 and 4, and
/// named destinations: a file with an outline and a form.
fn outline_and_form() -> Vec<u8> {
    let n = 8usize;
    let mut o: Vec<(u32, String)> = Vec::new();
    o.push((1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /Names << /Dests 6 0 R >> /AcroForm << /Fields [40 0 R 41 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 3 0 R >> >> >> /PageLabels << /Nums [0 << /S /D >>] >> >>".to_string()));
    let kids: String = (0..n).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    o.push((2, format!("<< /Type /Pages /Kids [{kids}] /Count {n} /Resources << /Font << /F1 3 0 R >> >> >>")));
    o.push((3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()));
    o.push((5, format!("<< /Type /Outlines /First 50 0 R /Last {} 0 R /Count {n} >>", 50 + n - 1)));
    let names: String = (0..n).map(|i| format!("(dest{i}) [{} 0 R /Fit] ", 10 + 2 * i)).collect();
    o.push((6, format!("<< /Names [{names}] >>")));
    for i in 0..n {
        let (page, content) = (10 + 2 * i as u32, 11 + 2 * i as u32);
        let annots = match i {
            1 => " /Annots [40 0 R 60 0 R 61 0 R]",
            4 => " /Annots [41 0 R 60 0 R]",
            _ => " /Annots [60 0 R]",
        };
        o.push((page, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 300] /Contents {content} 0 R{annots} >>", 200 + i)));
        let body = format!("BT /F1 24 Tf 20 100 Td (OUT-{i:04}) Tj ET");
        o.push((content, format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())));
        let prev = if i > 0 { format!(" /Prev {} 0 R", 49 + i) } else { String::new() };
        let next = if i + 1 < n { format!(" /Next {} 0 R", 51 + i) } else { String::new() };
        o.push((50 + i as u32, format!("<< /Title (P{i}) /Parent 5 0 R /Dest [{page} 0 R /Fit]{prev}{next} >>")));
    }
    o.push((40, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (first) /V (hello) /Rect [10 10 120 40] /P 12 0 R /DA (/Helv 12 Tf 0 g) >>".to_string()));
    o.push((41, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (second) /V (world) /Rect [10 10 120 40] /P 18 0 R /DA (/Helv 12 Tf 0 g) >>".to_string()));
    // A link on every page to the first page by name, and one on page 1 to page 6 by an explicit destination.
    o.push((60, "<< /Type /Annot /Subtype /Link /Rect [0 0 50 20] /Dest (dest0) >>".to_string()));
    o.push((61, "<< /Type /Annot /Subtype /Link /Rect [60 0 100 20] /Dest [22 0 R /Fit] >>".to_string()));
    assemble(&o, "/Root 1 0 R")
}

fn qpdf_make(qpdf: &Path, input: &Path, output: &Path, args: &[&str]) {
    let run = Command::new(qpdf).args(args).arg(input).arg(output).output().expect("cannot run qpdf");
    assert!(matches!(run.status.code(), Some(0 | 3)), "qpdf {args:?}: {}", String::from_utf8_lossy(&run.stderr));
}

// --- checks ----------------------------------------------------------------------------------------------------------------------------

/// What the PDFium script is to check: the case, as a line of JSON.
static MANIFEST: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn json(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "/").replace('"', "\\\""))
}

/// Say, for the other readers, what `output` must be: the model's pages, from `original` and `insert`.
fn expect_in_manifest(label: &str, original: &Path, original_pw: &str, output: &Path, output_pw: &str, insert: Option<(&Path, &str)>, model: &Model) {
    let pages: Vec<String> = model
        .pages
        .iter()
        .map(|(src, turn)| match src {
            Src::Orig(i) => format!("[\"o\",{i},{turn}]"),
            Src::Ins(j) => format!("[\"i\",{j},{turn}]"),
        })
        .collect();
    let (ins, ins_pw) = insert.map_or((String::new(), String::new()), |(p, pw)| (p.to_string_lossy().into_owned(), pw.to_string()));
    let line = format!(
        "{{\"label\":{},\"original\":{},\"original_password\":{},\"output\":{},\"output_password\":{},\"insert\":{},\"insert_password\":{},\"expect\":[{}]}}",
        json(label),
        json(&original.to_string_lossy()),
        json(original_pw),
        json(&output.to_string_lossy()),
        json(output_pw),
        json(&ins),
        json(&ins_pw),
        pages.join(",")
    );
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

/// The pages of `path` as our reader has them (sizes as shown).
fn reopened(path: &Path, password: &str) -> Vec<PageInfo> {
    Viewer::open(path, password).opened.pages
}

/// The whole round: the pages the engine told after the last call equal the model; everything undone, the file saved is the file it was,
/// and everything done again gives the pages again; saved, the bytes of the original are the beginning of the file, qpdf accepts it, our
/// reader opens it and has the model's pages; saved as a whole, the same pages.
#[allow(clippy::too_many_arguments)]
fn settle(label: &str, v: &mut Viewer, original: &Path, password: &str, model: &Model, told: &EditResult, insert: Option<(&Path, &str)>, base_sizes: &[PageInfo], ins_sizes: &[PageInfo]) {
    let want: Vec<(i64, i64)> = model
        .pages
        .iter()
        .map(|(src, turn)| match src {
            Src::Orig(i) => turned(base_sizes[*i], *turn),
            Src::Ins(j) => turned(ins_sizes[*j], *turn),
        })
        .collect();
    let told_sizes: Vec<(i64, i64)> = told.pages.as_ref().expect("a change to the pages tells the pages").iter().map(|p| (p.width.round() as i64, p.height.round() as i64)).collect();
    assert_eq!(told_sizes, want, "{label}: the pages the engine told");

    // Everything undone: the file saved is the file it was, byte for byte (nothing changed, so it is written as it is) ...
    let mut steps = 0;
    loop {
        let r = v.undo();
        if r.status == EditStatus::Nothing {
            break;
        }
        assert_eq!(r.status, EditStatus::Done, "{label}: {}", r.message);
        assert!(r.pages.is_some(), "{label}: undoing a change to the pages tells the pages");
        steps += 1;
        assert!(steps < 100);
    }
    let undone = out_dir().join(format!("{label}-undone.pdf"));
    let (status, message) = v.save(&undone);
    assert_eq!(status, SaveStatus::Done, "{label}: {message}");
    assert!(std::fs::read(&undone).unwrap() == std::fs::read(original).unwrap(), "{label}: the file with every change undone is not the file");
    // ... and done again, the pages are the pages (and the last page told is told again).
    let mut last_told = None;
    for _ in 0..steps {
        let r = v.redo();
        assert_eq!(r.status, EditStatus::Done, "{label}: {}", r.message);
        last_told = r.pages;
    }
    assert_eq!(v.redo().status, EditStatus::Nothing);
    assert_eq!(last_told.expect("steps were redone").iter().map(|p| (p.width.round() as i64, p.height.round() as i64)).collect::<Vec<_>>(), want, "{label}: the pages told after redo");

    let saved = out_dir().join(format!("{label}.pdf"));
    let (status, message) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{label}: {message}");
    let (before, after) = (std::fs::read(original).unwrap(), std::fs::read(&saved).unwrap());
    assert!(after.starts_with(&before) && after.len() > before.len(), "{label}: the original is not the beginning of the saved file");
    // One update only: one more `startxref` than the original has.
    let count = |bytes: &[u8]| bytes.windows(9).filter(|w| *w == b"startxref").count();
    assert_eq!(count(&after), count(&before) + 1, "{label}: more than one update was written");
    qpdf_accepts(&saved, original, password);
    let sizes: Vec<(i64, i64)> = reopened(&saved, password).iter().map(|p| (p.width.round() as i64, p.height.round() as i64)).collect();
    assert_eq!(sizes, want, "{label}: the saved file read again");
    let doc = Document::open_with_password(&saved, password).unwrap();
    assert!(!doc.was_repaired(), "{label}: the saved file needed its table rebuilt");
    expect_in_manifest(label, original, password, &saved, password, insert, model);

    // A whole rewrite is a file with the same pages.
    let whole = out_dir().join(format!("{label}-whole.pdf"));
    let mut again = Viewer::open(&saved, password);
    let (status, message) = again.save_as(&whole);
    assert_eq!(status, SaveStatus::Done, "{label}: {message}");
    qpdf_accepts(&whole, &saved, password);
    let sizes: Vec<(i64, i64)> = reopened(&whole, password).iter().map(|p| (p.width.round() as i64, p.height.round() as i64)).collect();
    assert_eq!(sizes, want, "{label}: save as");
    expect_in_manifest(&format!("{label}-whole"), original, password, &whole, password, insert, model);

}

// --- the cases ---------------------------------------------------------------------------------------------------------------------

/// Turn, take out, move and put in, in a way that touches every kind of page: each call checks the pages the engine tells; then `settle`.
fn rounds(label: &str, original: &Path, password: &str, source: &Path, source_pw: &str, source_pages: usize) {
    let mut v = Viewer::open(original, password);
    let base_sizes = v.opened.pages.clone();
    let n = base_sizes.len();
    assert!(n >= 5, "{label}: the file has {n} pages");
    let ins_sizes = Viewer::open(source, source_pw).opened.pages;
    assert_eq!(ins_sizes.len(), source_pages);
    let mut model = Model::new(n);
    let (mut last, mut steps) = (None, 0);
    let mut check = |v: &mut Viewer, model: &Model, r: EditResult| {
        assert_eq!(r.status, EditStatus::Done, "{label}: {}", r.message);
        assert!(r.dirty && r.can_undo && !r.can_redo, "{label}: {r:?}");
        let told = r.pages.as_ref().expect("the pages are told");
        assert_eq!(told.len(), model.pages.len(), "{label}: after step {}", steps);
        // The page in the middle can be drawn at its size: no bitmap is left from the page that was there before.
        let probe = model.pages.len() / 2;
        let id = v.id();
        v.engine.render(qingpdf_core::view::RenderRequest { doc: 1, id, page: probe as u32, dpi: 36.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
        let drawn = v.until(|e| match e {
            Event::Rendered(d) if d.id == id => Some(d),
            _ => None,
        });
        let want = turned(told[probe], 0);
        let (dw, dh) = (i64::from(drawn.width), i64::from(drawn.height));
        assert!((dw - want.0 / 2).abs() <= 1 && (dh - want.1 / 2).abs() <= 1, "{label}: the page drawn after step {steps} is {dw} x {dh}, the page is {want:?}");
        steps += 1;
        last = Some(r);
    };

    let r = v.rotate(&[1, 3], 90);
    model.rotate(&[1, 3], 90);
    check(&mut v, &model, r);
    let r = v.rotate(&[3], -180);
    model.rotate(&[3], -180);
    check(&mut v, &model, r);
    let r = v.delete(&[0, (n - 1) as u32]);
    model.delete(&[0, (n - 1) as u32]);
    check(&mut v, &model, r);
    let r = v.mv(&[1, 2], 0);
    model.mv(&[1, 2], 0);
    check(&mut v, &model, r);
    let r = v.insert(2, source, source_pw);
    model.insert(2, source_pages);
    check(&mut v, &model, r);
    let len = model.pages.len() as u32;
    let r = v.mv(&[0], len);
    model.mv(&[0], len);
    check(&mut v, &model, r);
    let r = v.insert(model.pages.len() as u32, source, source_pw);
    model.insert(model.pages.len(), source_pages);
    // (The second copy of the file is the same file: its pages are Ins(j) again, which is what the model says.)
    check(&mut v, &model, r);
    let told = last.clone().expect("steps were taken");
    settle(label, &mut v, original, password, &model, &told, Some((source, source_pw)), &base_sizes, &ins_sizes);
}

#[test]
fn a_hundred_pages_classic_xref_turned_deleted_moved_and_inserted() {
    let dir = out_dir();
    let original = dir.join("src-100.pdf");
    shared_file(&original, &synthetic(100, "ORIG"));
    let source = dir.join("src-7.pdf");
    shared_file(&source, &synthetic(7, "SRC"));
    rounds("classic-100", &original, "", &source, "", 7);
}

#[test]
fn a_hundred_pages_in_object_and_xref_streams_and_in_aes_256() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let dir = out_dir();
    let plain = dir.join("src-100.pdf");
    shared_file(&plain, &synthetic(100, "ORIG"));
    let source = dir.join("src-7.pdf");
    shared_file(&source, &synthetic(7, "SRC"));
    let streams = dir.join("src-100-objstm.pdf");
    qpdf_make(&qpdf, &plain, &streams, &["--object-streams=generate"]);
    assert!(Document::open(&streams).unwrap().uses_xref_streams());
    rounds("xref-stream-100", &streams, "", &source, "", 7);
    // AES-256 (revision 6), with a user password, and the object streams.
    let aes = dir.join("src-100-aes256.pdf");
    qpdf_make(&qpdf, &plain, &aes, &["--object-streams=generate", "--encrypt", "user-pw", "owner-pw", "256", "--"]);
    rounds("aes256-100", &aes, "user-pw", &source, "", 7);
    // Pages come in from an encrypted file as well (the one with the same protection or none: the file here is not restricted).
    let enc_source = dir.join("src-7-aes256.pdf");
    qpdf_make(&qpdf, &source, &enc_source, &["--encrypt", "u", "o", "256", "--"]);
    rounds("classic-from-aes256", &plain, "", &enc_source, "u", 7);
}

#[test]
fn real_files_of_the_corpus_keep_their_pages_in_order() {
    let dir = out_dir();
    let source = dir.join("src-7.pdf");
    shared_file(&source, &synthetic(7, "SRC"));
    for (label, rel, password) in [
        ("latex-arxiv", "zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf", ""),
        ("word-arxiv", "zh/lunwen/lunwen-arxiv-2401.01717-word.pdf", ""),
        ("work-report", "zh/gongwen/gongwen-2024-nicheng-work-report.pdf", ""),
        ("ebook-128", "zh/ebook/ebook-wikisource-minfadian.pdf", ""),
        ("rects-aes256", "encrypted/qpdf-generated/rects.r6-aes256-user-long.pdf", "correct horse battery staple, a very long password of more than thirty-two characters"),
    ] {
        rounds(label, &common::public_file(rel), password, &source, "", 7);
    }
}

#[test]
fn the_outline_the_links_and_the_form_follow_the_pages() {
    let dir = out_dir();
    let original = dir.join("outline-form.pdf");
    std::fs::write(&original, outline_and_form()).unwrap();
    let mut v = Viewer::open(&original, "");
    assert_eq!(v.opened.pages.len(), 8);
    // Where each bookmark leads (page numbers from 0), and where the explicit link on page 1 leads.
    let outline = |v: &mut Viewer| -> Vec<(String, Option<u32>)> {
        let id = v.id();
        v.engine.outline(1, id);
        v.until(|e| match e {
            Event::Outline { id: i, items, .. } if i == id => Some(items.into_iter().map(|it| (it.title, it.page)).collect()),
            _ => None,
        })
    };
    let link_targets = |v: &mut Viewer, page: u32| -> Vec<u32> {
        let id = v.id();
        v.engine.links(1, id, page);
        v.until(|e| match e {
            Event::Links { id: i, links, .. } if i == id => Some(links.into_iter().filter(|l| l.kind == LinkKind::Page).map(|l| l.page).collect()),
            _ => None,
        })
    };
    let titles: Vec<(String, Option<u32>)> = (0..8).map(|i| (format!("P{i}"), Some(i))).collect();
    assert_eq!(outline(&mut v), titles);
    // Page 1 is moved to the end; page 5 goes; the bookmarks follow their pages and the one of the page that went leads nowhere.
    let r = v.mv(&[1], 8);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.delete(&[4]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    // Now: 0 2 3 4(old 5) ... let the model say: [0,2,3,4,5,6,7,1] minus new index 4 (old 5) = [0,2,3,4,6,7,1].
    let order = [0u32, 2, 3, 4, 6, 7, 1];
    let now = outline(&mut v);
    for (title, page) in &now {
        let old: u32 = title[1..].parse().unwrap();
        let expected = order.iter().position(|&o| o == old).map(|p| p as u32);
        assert_eq!(*page, expected, "bookmark {title}");
    }
    assert!(now.iter().any(|(t, p)| t == "P5" && p.is_none()), "the bookmark of the page that went leads nowhere");
    // The link on old page 1 (now the last, index 6) to old page 6 by an explicit destination follows it, and the named one goes to page 0.
    let on_moved = link_targets(&mut v, 6);
    assert!(on_moved.contains(&4) && on_moved.contains(&0), "{on_moved:?}");
    // The field of old page 4 (index 3) is in the form; the one of page 1 is too (it moved). Delete page 4's page: its field goes.
    let fields = |v: &mut Viewer, page: u32| {
        let id = v.id();
        v.engine.fields(1, id, page);
        v.until(|e| match e {
            Event::Fields { id: i, items, .. } if i == id => Some(items.len()),
            _ => None,
        })
    };
    assert_eq!((fields(&mut v, 6), fields(&mut v, 3)), (1, 1));
    let r = v.delete(&[3]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let saved = dir.join("outline-form-after.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let doc = Document::open(&saved).unwrap();
    let catalog = doc.catalog().unwrap();
    let acro = doc.resolve(catalog.get("AcroForm").unwrap()).unwrap();
    let fields_left = doc.resolve(acro.as_dict().unwrap().get("Fields").unwrap()).unwrap();
    assert_eq!(fields_left.as_array().unwrap().len(), 1, "the field of the page that went is out of the form");
    assert!(!catalog.contains_key("PageLabels"), "the page labels do not line up any more");
    qpdf_accepts(&saved, &original, "");
    // Save as: the pages that went are really gone (their words are in the earlier part of the saved file, not in the whole one).
    let whole = dir.join("outline-form-whole.pdf");
    assert_eq!(v.save_as(&whole).0, SaveStatus::Done);
    let (kept, rewritten) = (std::fs::read(&saved).unwrap(), std::fs::read(&whole).unwrap());
    assert!(common::contains(&kept, b"OUT-0005") && common::contains(&kept, b"OUT-0004"));
    assert!(!common::contains(&rewritten, b"OUT-0005") && !common::contains(&rewritten, b"OUT-0004") && common::contains(&rewritten, b"OUT-0006"));
    qpdf_accepts(&whole, &saved, "");
    let mut model = Model::new(8);
    model.mv(&[1], 8);
    model.delete(&[4]);
    model.delete(&[3]);
    expect_in_manifest("outline-form", &original, "", &saved, "", None, &model);
    expect_in_manifest("outline-form-whole", &original, "", &whole, "", None, &model);
    // Pages put in from a file with fields: the fields are in the form, with names that do not clash.
    let with_forms = common::public_file("outline-form-attach/multiple_form_types.pdf");
    let mut v = Viewer::open(&original, "");
    let r = v.insert(1, &with_forms, "");
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let again = dir.join("outline-form-inserted.pdf");
    assert_eq!(v.save(&again).0, SaveStatus::Done);
    qpdf_accepts(&again, &original, "");
    let mut model = Model::new(8);
    model.insert(1, 1);
    expect_in_manifest("outline-form-inserted", &original, "", &again, "", Some((&with_forms, "")), &model);
    let doc = Document::open(&again).unwrap();
    let acro = doc.resolve(doc.catalog().unwrap().get("AcroForm").unwrap()).unwrap();
    let fields = doc.resolve(acro.as_dict().unwrap().get("Fields").unwrap()).unwrap();
    assert!(fields.as_array().unwrap().len() > 2, "the fields of the file came into the form");
}

#[test]
fn what_may_not_be_changed_is_not() {
    let dir = out_dir();
    // Signed files: a signature field with a value, and a certified file: nothing is done to the pages.
    for rel in ["incremental/two_signatures.pdf", "incremental/signature_reason.pdf"] {
        let mut v = Viewer::open(&common::public_file(rel), "");
        let r = v.rotate(&[0], 90);
        assert_eq!(r.status, EditStatus::Failed, "{rel}: {r:?}");
        assert!(r.message.contains("signed") || r.message.contains("certified"), "{rel}: {}", r.message);
        assert!(!r.dirty && r.pages.is_none());
        if v.opened.pages.len() > 1 {
            assert_eq!(v.delete(&[0]).status, EditStatus::Failed);
        }
    }
    // Assembling not allowed (permission bit 11): the user password is refused, the owner password may.
    let file = common::public_file("encrypted/qpdf-generated/bookmarks.r4-aes128-user-assemble-n.pdf");
    let mut v = Viewer::open(&file, "user");
    assert!(!v.opened.rights.assemble);
    let r = v.rotate(&[0], 90);
    assert_eq!(r.status, EditStatus::Denied, "{r:?}");
    assert_eq!(v.mv(&[0], 2).status, EditStatus::Denied);
    assert_eq!(v.delete(&[0]).status, EditStatus::Denied);
    let owner = Viewer::open(&file, "owner");
    assert!(owner.opened.rights.assemble);
    let mut owner = owner;
    assert_eq!(owner.rotate(&[0], 90).status, EditStatus::Done);
    // Every permission denied: extracting is refused too, with the owner password it works.
    let denied = common::public_file("encrypted/qpdf-generated/two.r6-aes256-user-everything-denied.pdf");
    let mut v = Viewer::open(&denied, "user");
    let (status, message, _) = v.extract(&[0], &dir.join("must-not-exist.pdf"));
    assert_eq!(status, SaveStatus::Failed, "{message}");
    assert!(!dir.join("must-not-exist.pdf").exists());
    // Not all the pages, not a page that is not there, not the same place.
    let mut v = Viewer::open(&common::public_file("page-tree/hello_world_rotated.pdf"), "");
    let n = v.opened.pages.len() as u32;
    assert_eq!(v.delete(&(0..n).collect::<Vec<_>>()).status, EditStatus::Failed);
    assert_eq!(v.delete(&[n + 3]).status, EditStatus::Failed);
    assert_eq!(v.mv(&[1], 2).status, EditStatus::Failed, "page 1 in front of page 2 is where it is");
    assert_eq!(v.rotate(&[0], 0).status, EditStatus::Failed);
    assert_eq!(v.undo().status, EditStatus::Nothing, "failed changes leave nothing to undo");
    // Pages from a file that does not exist, is not a PDF, is restricted, needs a password.
    let mut v = Viewer::open(&common::public_file("page-tree/hello_world_rotated.pdf"), "");
    assert_eq!(v.insert(0, &dir.join("no-such.pdf"), "").status, EditStatus::Failed);
    let junk = dir.join("junk.pdf");
    std::fs::write(&junk, b"nothing like a PDF").unwrap();
    assert_eq!(v.insert(0, &junk, "").status, EditStatus::Failed);
    let restricted = common::public_file("encrypted/qpdf-generated/bookmarks.r3-128-rc4-user-modify-none.pdf");
    assert_eq!(v.insert(0, &restricted, "").status, EditStatus::NeedsPassword);
    assert_eq!(v.insert(0, &restricted, "wrong").status, EditStatus::WrongPassword);
    let refused = v.insert(0, &restricted, "user");
    assert_eq!(refused.status, EditStatus::Failed, "{refused:?}");
    assert!(refused.message.contains("restricts"), "{}", refused.message);
    assert_eq!(v.insert(0, &restricted, "owner").status, EditStatus::Done);
    assert!(!v.undo().pages.is_none());
}

#[test]
fn extracted_pages_are_a_file_of_their_own_with_the_protection_of_the_file() {
    let dir = out_dir();
    let original = dir.join("src-100.pdf");
    shared_file(&original, &synthetic(100, "ORIG"));
    let mut v = Viewer::open(&original, "");
    // The edits not saved are in the file made.
    assert_eq!(v.rotate(&[3], 90).status, EditStatus::Done);
    let target = dir.join("extract-3-4-10.pdf");
    let (status, message, pages) = v.extract(&[10, 3, 4, 3], &target);
    assert_eq!((status, pages), (SaveStatus::Done, 3), "{message}");
    qpdf_accepts(&target, &original, "");
    let again = Viewer::open(&target, "");
    // (The page turned a quarter has its sides the other way round; page 3 is 203 wide.)
    let sizes: Vec<(i64, i64)> = again.opened.pages.iter().map(|p| (p.width.round() as i64, p.height.round() as i64)).collect();
    assert_eq!(sizes, [(300, 203), (204, 300), (210, 300)]);
    // The open file is not touched.
    assert_eq!(std::fs::read(&original).unwrap(), synthetic(100, "ORIG"));
    expect_in_manifest("extract", &original, "", &target, "", None, &{
        let mut m = Model::new(100);
        m.rotate(&[3], 90);
        Model { pages: vec![m.pages[3], m.pages[4], m.pages[10]] }
    });
    // A file with a password is extracted into a file with the same password.
    let Some(qpdf) = common::find_qpdf() else { return };
    let aes = dir.join("extract-src-aes256.pdf");
    qpdf_make(&qpdf, &original, &aes, &["--encrypt", "user-pw", "owner-pw", "256", "--"]);
    let mut v = Viewer::open(&aes, "user-pw");
    let target = dir.join("extract-aes256.pdf");
    let (status, message, pages) = v.extract(&[0, 99], &target);
    assert_eq!((status, pages), (SaveStatus::Done, 2), "{message}");
    qpdf_accepts(&target, &aes, "user-pw");
    assert_eq!(Viewer::open(&target, "user-pw").opened.pages.len(), 2);
    let locked = Viewer::open_unlocked_result(&target);
    assert!(locked, "the extracted file needs the password too");
}

impl Viewer {
    /// Does opening `path` without a password fail with `NeedsPassword`?
    fn open_unlocked_result(path: &Path) -> bool {
        let engine = qingpdf_core::view::Engine::start(Box::new(|| {}));
        engine.open_file(1, &path.to_string_lossy(), "", qingpdf_core::view::TOTAL_BYTES, 1920 * 1080);
        matches!(engine.wait(20_000), Some(Event::OpenFailed { failure: qingpdf_core::view::OpenFailure::NeedsPassword, .. }))
    }
}

#[test]
fn a_file_with_a_damaged_table_is_written_afresh_before_the_first_change_and_the_change_is_made() {
    let dir = out_dir();
    let mut bytes = synthetic(6, "BROKEN");
    // The cross-reference table is not where the file says (and the file says it in a way no reader trusts).
    let at = bytes.windows(9).rposition(|w| w == b"startxref").unwrap();
    bytes.truncate(at);
    bytes.extend_from_slice(b"startxref\n7\n%%EOF\n");
    let broken = dir.join("broken-6.pdf");
    std::fs::write(&broken, &bytes).unwrap();
    let mut v = Viewer::open(&broken, "");
    assert_eq!(v.opened.pages.len(), 6);
    let r = v.rotate(&[2], 90);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(r.rewrote);
    assert_eq!(r.pages.as_ref().unwrap().len(), 6);
    let r = v.delete(&[0]);
    assert_eq!((r.status, r.pages.as_ref().map(Vec::len)), (EditStatus::Done, Some(5)));
    let saved = dir.join("broken-6-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    assert_eq!(reopened(&saved, "").len(), 5);
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = common::qpdf_check(&qpdf, &saved);
        assert!(matches!(verdict.code, 0 | 3), "{}", verdict.text);
    }
}

#[test]
fn the_speed_of_the_changes_on_a_thousand_pages_a_hundred_and_five_hundred() {
    let dir = out_dir();
    let big = dir.join("speed-1000.pdf");
    std::fs::write(&big, synthetic(1000, "BIG")).unwrap();
    let mut v = Viewer::open(&big, "");
    // Each change, answered (the pages told), best of three: the work of the engine; drawing is the reader's.
    let mut best = |f: &mut dyn FnMut(&mut Viewer) -> EditResult| {
        (0..3)
            .map(|_| {
                let t = Instant::now();
                let r = f(&mut v);
                assert_eq!(r.status, EditStatus::Done, "{}", r.message);
                assert!(r.pages.is_some());
                t.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::MAX, f64::min)
    };
    let rotate = best(&mut |v| v.rotate(&[10, 500, 900], 90));
    let delete = best(&mut |v| v.delete(&[1, 2, 3]));
    let reorder = best(&mut |v| v.mv(&[5, 6, 7], 800));
    println!("1000 pages: turn {rotate:.1} ms, delete {delete:.1} ms, move {reorder:.1} ms");
    assert!(rotate <= 100.0 && delete <= 100.0 && reorder <= 100.0, "turn {rotate:.1} delete {delete:.1} move {reorder:.1} ms");

    // Saving after changes to a file of a hundred pages.
    let hundred = dir.join("speed-100.pdf");
    std::fs::write(&hundred, synthetic(100, "HUN")).unwrap();
    let mut v = Viewer::open(&hundred, "");
    assert_eq!(v.rotate(&[3, 4], 90).status, EditStatus::Done);
    assert_eq!(v.delete(&[10]).status, EditStatus::Done);
    assert_eq!(v.mv(&[20, 21], 0).status, EditStatus::Done);
    let out = dir.join("speed-100-saved.pdf");
    let t = Instant::now();
    assert_eq!(v.save(&out).0, SaveStatus::Done);
    let save = t.elapsed().as_secs_f64() * 1000.0;
    println!("save after changes to 100 pages: {save:.1} ms");
    assert!(save <= 200.0, "save {save:.1} ms");

    // Putting in a file of 500 pages (about as heavy as a book of text: a page of a few hundred words).
    let words: String = (0..300).map(|i| format!("word{i} ")).collect::<String>();
    let mut objects: Vec<(u32, String)> = vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string())];
    let kids: String = (0..500).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    objects.push((2, format!("<< /Type /Pages /Kids [{kids}] /Count 500 /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> >>")));
    objects.push((3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()));
    for i in 0..500u32 {
        objects.push((10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i)));
        let body = format!("BT /F1 10 Tf 20 700 Td ({words}{i}) Tj ET");
        objects.push((11 + 2 * i, format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())));
    }
    let book = dir.join("speed-book-500.pdf");
    std::fs::write(&book, assemble(&objects, "/Root 1 0 R")).unwrap();
    let mut v = Viewer::open(&hundred, "");
    let t = Instant::now();
    let r = v.insert(50, &book, "");
    let insert = t.elapsed().as_secs_f64() * 1000.0;
    println!("put in 500 pages: {insert:.1} ms (status {:?}, {} bytes of file)", r.status, std::fs::metadata(&book).unwrap().len());
    assert_eq!((r.status, r.pages.as_ref().map(Vec::len)), (EditStatus::Done, Some(600)), "{}", r.message);
    assert!(insert <= 2000.0, "insert {insert:.1} ms");

    // ... and a real book of more than 500 pages (three books of the local corpus joined; skipped where they are not).
    let books: Vec<PathBuf> = ["large/freeculture.pdf", "../public/zh/ebook/ebook-wikisource-minfadian.pdf", "large/pdkids.pdf"].iter().map(|r| common::corpus_root().join("local").join(r)).collect();
    if books.iter().all(|b| b.is_file()) {
        let docs: Vec<Document> = books.iter().map(|b| Document::open(b).unwrap()).collect();
        let names: Vec<String> = books.iter().map(|b| b.display().to_string()).collect();
        let inputs: Vec<qingpdf_core::ops::Input<'_>> = docs.iter().zip(&names).map(|(doc, name)| qingpdf_core::ops::Input { name, doc, password: "" }).collect();
        let merged = qingpdf_core::ops::merge(&inputs).unwrap();
        let real = dir.join("speed-real-book.pdf");
        shared_file(&real, &merged.data);
        let mut v = Viewer::open(&hundred, "");
        let t = Instant::now();
        let r = v.insert(50, &real, "");
        let took = t.elapsed().as_secs_f64() * 1000.0;
        println!("put in a real book of {} pages ({} bytes): {took:.1} ms (status {:?})", merged.pages, merged.data.len(), r.status);
        assert_eq!((r.status, r.pages.as_ref().map(Vec::len)), (EditStatus::Done, Some(100 + merged.pages)), "{}", r.message);
        assert!(took <= 2000.0, "insert of a real book {took:.1} ms");
    }
}

/// Open a file the way a person does: `None` if it does not open (damaged beyond repair, locked).
fn try_open(path: &Path, password: &str) -> Option<Viewer> {
    let engine = qingpdf_core::view::Engine::start(Box::new(|| {}));
    engine.open_file(1, &path.to_string_lossy(), password, qingpdf_core::view::TOTAL_BYTES, 1920 * 1080);
    match engine.wait(60_000) {
        Some(Event::Opened(opened)) => Some(Viewer { engine, opened, next: 100 }),
        _ => None,
    }
}

#[test]
fn every_file_of_the_corpus_that_opens_takes_the_changes_or_refuses_them_with_a_reason() {
    let out = common::fresh_out_dir("pages-corpus");
    let source = out.join("src-3.pdf");
    std::fs::write(&source, synthetic(3, "SRC")).unwrap();
    let qpdf = common::find_qpdf();
    let (mut done, mut skipped, mut quirks, mut refused) = (0usize, 0usize, 0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();
    for (k, input) in common::corpus_files().iter().enumerate() {
        let name = common::corpus_name(input);
        let Some(mut v) = try_open(input, &common::password_for(input)) else {
            skipped += 1;
            continue;
        };
        let n = v.opened.pages.len() as u32;
        if n == 0 {
            skipped += 1;
            continue;
        }
        let mut told: Option<Vec<PageInfo>> = None;
        let mut results: Vec<(&str, EditResult)> = Vec::new();
        results.push(("rotate", v.rotate(&[0], 90)));
        if n >= 2 {
            results.push(("move", v.mv(&[0], n)));
            results.push(("delete", v.delete(&[n - 1])));
        }
        results.push(("insert", v.insert(0, &source, "")));
        let mut bad = false;
        for (what, r) in results {
            match r.status {
                EditStatus::Done => {
                    if r.pages.is_none() || !r.dirty {
                        failures.push(format!("{name}: {what} is done but says nothing of the pages"));
                        bad = true;
                    }
                    told = r.pages.or(told);
                }
                EditStatus::Denied => refused += 1,
                // A signed file, a page tree that cannot be changed (a page listed twice, a stream for a page): refused, and said why.
                EditStatus::Failed if ["signed", "certified", "twice", "stream", "page tree", "no pages", "needs at least"].iter().any(|w| r.message.contains(w)) => refused += 1,
                other => {
                    failures.push(format!("{name}: {what}: {other:?} {}", r.message));
                    bad = true;
                }
            }
        }
        if bad {
            continue;
        }
        let Some(told) = told else { continue };
        let saved = out.join(format!("{k:03}.pdf"));
        let (status, message) = v.save(&saved);
        if status != SaveStatus::Done {
            failures.push(format!("{name}: save: {message}"));
            continue;
        }
        // Our own reader opens the file again with the pages the engine told.
        let sizes = |pages: &[PageInfo]| pages.iter().map(|p| (p.width.round() as i64, p.height.round() as i64)).collect::<Vec<_>>();
        match try_open(&saved, &common::password_for(input)) {
            Some(again) if sizes(&again.opened.pages) == sizes(&told) => {}
            Some(again) => {
                failures.push(format!("{name}: the saved file has {} pages, {} were told", again.opened.pages.len(), told.len()));
                continue;
            }
            None => {
                failures.push(format!("{name}: the saved file does not open"));
                continue;
            }
        }
        if let Some(qpdf) = &qpdf {
            match common::judge_qpdf_output(qpdf, &saved, &[input.as_path()], false) {
                common::Judgement::Clean => {}
                common::Judgement::InputQuirk => quirks += 1,
                common::Judgement::Fail(why) => {
                    failures.push(format!("{name}: qpdf: {why}"));
                    continue;
                }
            }
        }
        done += 1;
    }
    println!("{done} files changed and saved ({quirks} with only the input's own qpdf warnings), {refused} changes refused with a reason, {skipped} files skipped (locked or unopenable)");
    for f in &failures {
        println!("FAIL: {f}");
    }
    assert!(failures.is_empty(), "{} file(s) failed", failures.len());
    assert!(done > 50, "only {done} files were changed");
}

#[test]
fn a_file_with_an_outline_and_a_form_goes_through_all_the_changes_and_is_checked_by_every_reader() {
    let dir = out_dir();
    let original = dir.join("outline-form-rounds-src.pdf");
    shared_file(&original, &outline_and_form());
    let source = dir.join("src-7.pdf");
    shared_file(&source, &synthetic(7, "SRC"));
    rounds("outline-form-rounds", &original, "", &source, "", 7);
    // The outline of a file that is put in is not put in: the bookmarks are still the file's own.
    let mut v = Viewer::open(&original, "");
    let with_outline = common::public_file("outline-form-attach/bookmarks.pdf");
    let r = v.insert(2, &with_outline, "");
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let id = v.id();
    v.engine.outline(1, id);
    let titles: Vec<(String, Option<u32>)> = v.until(|e| match e {
        Event::Outline { id: i, items, .. } if i == id => Some(items.into_iter().map(|it| (it.title, it.page)).collect()),
        _ => None,
    });
    // (The pages of the file's bookmarks after the two pages that came in at 2.)
    let expect: Vec<(String, Option<u32>)> = (0..8u32).map(|i| (format!("P{i}"), Some(if i < 2 { i } else { i + 2 }))).collect();
    assert_eq!(titles, expect);
}

#[test]
fn what_was_done_to_a_page_stays_with_that_page_wherever_it_goes_and_comes_back_with_an_undo() {
    let dir = out_dir();
    let original = dir.join("outline-form-stay.pdf");
    shared_file(&original, &outline_and_form());
    let mut v = Viewer::open(&original, "");
    let search = |v: &mut Viewer, query: &str| -> Vec<u32> {
        let id = v.id();
        v.engine.search(1, id, query, 0);
        let mut pages = Vec::new();
        v.until(|e| match e {
            Event::SearchHits { id: i, page, .. } if i == id => {
                pages.push(page);
                None
            }
            Event::SearchDone { id: i, .. } if i == id => Some(()),
            _ => None,
        });
        pages
    };
    assert_eq!(search(&mut v, "OUT-0004"), [4]);
    // A note on page 4, a text line changed on page 6, a field filled in on page 1: all by the page's own objects.
    let r = v.pages_done(|e, id| e.add_note(1, id, 4, [100.0, 100.0], "stays", [255, 215, 0], "T"));
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let id = v.id();
    v.engine.text_line(1, id, 6, [30.0, 190.0]);
    let (line, rect) = v.until(|e| match e {
        Event::TextLine { id: i, line, .. } if i == id => Some(line.map(|l| (l.text, l.rect))),
        _ => None,
    })
    .expect("the line of page 6");
    assert_eq!(line, "OUT-0006");
    let r = v.pages_done(|e, id| e.replace_line(1, id, 6, [rect[0] + 2.0, (rect[1] + rect[3]) / 2.0], "OUT-0006", "CHANGED-6"));
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let id = v.id();
    v.engine.fields(1, id, 1);
    let field = v.until(|e| match e {
        Event::Fields { id: i, items, .. } if i == id => Some(items.into_iter().next()),
        _ => None,
    })
    .expect("a field on page 1");
    let r = v.pages_done(|e, id| e.fill_field(1, id, 1, field.index, field.num, qingpdf_core::view::FillValue::Text("filled".to_string())));
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    // Pages 4 and 6 and 1 are moved to the front, in that order of the document: now 1, 4, 6 are pages 0, 1, 2; the rest follow.
    let r = v.mv(&[1, 4, 6], 0);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let notes = |v: &mut Viewer, page: u32| -> usize {
        let id = v.id();
        v.engine.annotations(1, id, page);
        v.until(|e| match e {
            Event::Annotations { id: i, items, .. } if i == id => Some(items.iter().filter(|a| a.subtype == "Text").count()),
            _ => None,
        })
    };
    assert_eq!((notes(&mut v, 1), notes(&mut v, 4), notes(&mut v, 0)), (1, 0, 0), "the note went with its page (old page 4 is page 1 now)");
    assert_eq!(search(&mut v, "OUT-0004"), [1], "what the search finds is by the new places");
    assert_eq!(search(&mut v, "CHANGED-6"), [2], "the changed text is on its page");
    assert!(search(&mut v, "OUT-0006").is_empty(), "the old words are gone from the page");
    let id = v.id();
    v.engine.fields(1, id, 0);
    let (value, count) = v.until(|e| match e {
        Event::Fields { id: i, items, .. } if i == id => Some((items.first().map(|f| f.value.clone()), items.len())),
        _ => None,
    });
    assert_eq!((value.as_deref(), count), (Some("filled"), 1), "the filled field is on the page that moved");
    // Take the page with the note out: it is gone, and the undo brings it back with the note, the change of text and the field in it.
    let r = v.delete(&[1]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(search(&mut v, "OUT-0004").is_empty() && notes(&mut v, 1) == 0);
    assert_eq!(v.undo().status, EditStatus::Done);
    assert_eq!((notes(&mut v, 1), search(&mut v, "OUT-0004")), (1, vec![1]));
    // Saved, the file has all of it, in the new order.
    let saved = dir.join("outline-form-stay-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    qpdf_accepts(&saved, &original, "");
    let mut again = Viewer::open(&saved, "");
    assert_eq!((notes(&mut again, 1), search(&mut again, "CHANGED-6")), (1, vec![2]));
}

#[test]
fn a_file_with_too_many_pages_or_too_many_bytes_is_not_put_in() {
    let dir = out_dir();
    let mut v = Viewer::open(&{
        let p = dir.join("limits-base.pdf");
        shared_file(&p, &synthetic(3, "BASE"));
        p
    }, "");
    let big = dir.join("limits-5001.pdf");
    shared_file(&big, &synthetic(qingpdf_core::view::MAX_INSERT_PAGES + 1, "MANY"));
    let r = v.insert(0, &big, "");
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    assert!(r.message.contains("not put in at once"), "{}", r.message);
    // A file of more bytes than may be put in (a sparse file, so that nothing is written): refused at the size, unread.
    let heavy = dir.join("limits-bytes.pdf");
    let file = std::fs::File::create(&heavy).unwrap();
    file.set_len(qingpdf_core::view::MAX_INSERT_FILE_BYTES + 1).unwrap();
    drop(file);
    let r = v.insert(0, &heavy, "");
    assert_eq!(r.status, EditStatus::TooBig, "{r:?}");
    assert!(r.message.contains("do not fit"), "{}", r.message);
    let _ = std::fs::remove_file(&heavy);
    // Nothing was done: nothing to undo, not changed.
    assert_eq!(v.undo().status, EditStatus::Nothing);
    // The most (5000 pages) is put in, and takes what it needs.
    let five = dir.join("limits-5000.pdf");
    shared_file(&five, &synthetic(qingpdf_core::view::MAX_INSERT_PAGES, "FIVE"));
    let r = v.insert(3, &five, "");
    assert_eq!((r.status, r.pages.as_ref().map(Vec::len)), (EditStatus::Done, Some(5003)), "{}", r.message);
}
