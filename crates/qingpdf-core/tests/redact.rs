//! Redaction (item 8) through the engine's messages (the reader): marks made by a drag and by a find, listed as annotations, undone, saved with the
//! file; and applied (`Engine::redact`), which writes a new file with what lay under the marks taken out, and refuses what may not be changed. The
//! engine's own tests (`edit/redact_tests.rs`) look at the bytes; the other readers are checked by `tests/tools/redact_check.py`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::Viewer;

use std::path::PathBuf;

use qingpdf_core::Document;
use qingpdf_core::text::TextExtractor;
use qingpdf_core::view::{AnnotInfo, EditResult, EditStatus, Event, FindSpec, Preset, RedactHow, SaveStatus, ShapeKind};

impl Viewer {
    fn annotations(&mut self, page: u32) -> Vec<AnnotInfo> {
        let id = self.id();
        self.engine.annotations(1, id, page);
        self.until(|e| match e {
            Event::Annotations { id: i, items, .. } if i == id => Some(items),
            _ => None,
        })
    }

    fn mark_find(&mut self, words: &[&str], presets: &[Preset]) -> EditResult {
        let id = self.id();
        self.engine.redact_find(1, id, FindSpec { words: words.iter().map(ToString::to_string).collect(), presets: presets.to_vec() }, "Tester");
        self.edited(id)
    }

    fn mark_box(&mut self, page: u32, rect: [f32; 4]) -> EditResult {
        let id = self.id();
        self.engine.add_shape(1, id, page, ShapeKind::Redact, [rect[0], rect[1]], [rect[2], rect[3]], [255, 0, 0], 1.0, false, "Tester");
        self.edited(id)
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
        self.edited(id)
    }

    fn apply(&mut self, path: &std::path::Path, how: RedactHow) -> (SaveStatus, String, u64, u32, u32) {
        let id = self.id();
        self.engine.redact(1, id, &path.to_string_lossy(), how);
        self.until(|e| match e {
            Event::Redacted { id: i, status, message, bytes, pages, marks, .. } if i == id => Some((status, message, bytes, pages, marks)),
            _ => None,
        })
    }
}

fn gongwen() -> PathBuf {
    common::public_file("zh/gongwen/gongwen-2024-nicheng-work-report.pdf")
}

fn out_dir() -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("redact-engine");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn page_text(path: &std::path::Path, page: usize) -> String {
    let doc = Document::open(path).unwrap();
    let pages = doc.pages().unwrap();
    TextExtractor::new(&doc).page_text(&pages[page]).unwrap()
}

#[test]
fn marks_are_annotations_that_are_listed_undone_and_applied_and_the_open_file_is_not_touched() {
    let file = gongwen();
    let before = std::fs::read(&file).unwrap();
    let mut v = Viewer::open(&file, "");
    assert!(page_text(&file, 2).contains("全年税收收入预计完成"));
    // A find: one mark for each place (the answer says how many), on page 3 (index 2).
    let r = v.mark_find(&["全年税收收入预计完成"], &[]);
    assert_eq!((r.status, r.message.as_str()), (EditStatus::Done, "1"), "{r:?}");
    let list = v.annotations(2);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].subtype, "Redact");
    assert!(list[0].rect[2] > list[0].rect[0] && !list[0].parts.is_empty());
    // A box dragged out (the shape tool with the Redact kind): a second mark, on another page.
    let r = v.mark_box(0, [50.0, 100.0, 250.0, 140.0]);
    assert_eq!(r.status, EditStatus::Done, "{r:?}");
    assert_eq!(v.annotations(0).iter().filter(|a| a.subtype == "Redact").count(), 1);
    // Undone like any edit.
    assert_eq!(v.undo().status, EditStatus::Done);
    assert!(v.annotations(0).is_empty());
    // A box too small is not a mark.
    let r = v.mark_box(0, [50.0, 100.0, 51.0, 101.0]);
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    // Nothing found: a failure that says so.
    let r = v.mark_find(&["这句话不在文件里"], &[]);
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    // Applied: the new file has no mark and not the text; the open file is as it was; the marks are still in the document.
    let out = out_dir().join("gongwen-redacted.pdf");
    let (status, message, bytes, pages, marks) = v.apply(&out, RedactHow { label: "已涂黑".to_string(), clear_metadata: true, remove_active: false });
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert_eq!((pages, marks), (1, 1));
    assert_eq!(bytes, std::fs::metadata(&out).unwrap().len());
    assert_eq!(std::fs::read(&file).unwrap(), before, "the open file is not touched");
    let text = page_text(&out, 2);
    assert!(!text.contains("全年税收") && text.contains("地方财政收入"), "{text}");
    assert!(Document::open(&out).unwrap().pages().unwrap().iter().all(|p| p.dict.get("Annots").is_none()));
    assert_eq!(v.annotations(2).len(), 1, "the marks are still in the open document");
    if let Some(qpdf) = common::find_qpdf() {
        assert!(common::qpdf_check(&qpdf, &out).code != 2);
    }
    // Over the open file's own name (the temporary file is renamed over it): fine too.
    let copy = out_dir().join("copy.pdf");
    std::fs::copy(&file, &copy).unwrap();
    let mut w = Viewer::open(&copy, "");
    assert_eq!(w.mark_find(&["全年税收收入预计完成"], &[]).status, EditStatus::Done);
    let (status, message, ..) = w.apply(&copy, RedactHow::default());
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert!(!page_text(&copy, 2).contains("全年税收"));
}

#[test]
fn a_file_with_no_marks_or_that_may_not_be_changed_is_refused_with_a_reason() {
    let file = gongwen();
    let mut v = Viewer::open(&file, "");
    let out = out_dir().join("no-marks.pdf");
    let (status, message, ..) = v.apply(&out, RedactHow::default());
    assert_eq!(status, SaveStatus::Failed);
    assert!(message.contains("nothing to redact") && !out.exists(), "{message}");
    // A file whose author did not allow changing its content (AES-256, owner password needed): refused when it was opened with the user's.
    let Some(qpdf) = common::find_qpdf() else { return };
    let locked = out_dir().join("locked.pdf");
    let r = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner", "256", "--modify=none", "--"]).arg(&file).arg(&locked).output().unwrap();
    assert!(r.status.success() || r.status.code() == Some(3), "{}", String::from_utf8_lossy(&r.stderr));
    let mut w = Viewer::open(&locked, "");
    assert_eq!(w.mark_find(&["全年税收收入预计完成"], &[]).status, EditStatus::Denied);
    let (status, message, ..) = w.apply(&out_dir().join("locked-out.pdf"), RedactHow::default());
    assert_eq!(status, SaveStatus::Failed);
    assert!(message.contains("did not allow"), "{message}");
}
