//! Step 5a through the engine's messages (the reader): the line under a click, changing it, undoing it, saving it, and the files that
//! may not be changed. The engine's own tests (`edit/retext_tests.rs`) look at the bytes; the other readers are checked by
//! `tests/tools/replace_compare.py`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use qingpdf_core::text::TextExtractor;
use qingpdf_core::view::{EditResult, EditStatus, Engine, Event, LineView, SaveStatus, TOTAL_BYTES, TextStatus};
use qingpdf_core::Document;

struct Viewer {
    engine: Engine,
    next: u64,
}

impl Viewer {
    fn open(path: &Path, password: &str) -> Viewer {
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &path.to_string_lossy(), password, TOTAL_BYTES, 1920 * 1080);
        match engine.wait(20_000) {
            Some(Event::Opened(_)) => Viewer { engine, next: 100 },
            other => panic!("{}: not opened: {other:?}", path.display()),
        }
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn until<T>(&self, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        loop {
            match self.engine.wait(30_000) {
                Some(e) => {
                    if let Some(t) = pick(e) {
                        return t;
                    }
                }
                None => panic!("the engine said nothing for thirty seconds"),
            }
        }
    }

    fn line(&mut self, at: [f32; 2]) -> (TextStatus, String, Option<LineView>) {
        let id = self.id();
        self.engine.text_line(1, id, 0, at);
        self.until(|e| match e {
            Event::TextLine { id: got, status, message, line, .. } if got == id => Some((status, message, line)),
            _ => None,
        })
    }

    fn replace(&mut self, at: [f32; 2], old: &str, text: &str) -> EditResult {
        let id = self.id();
        self.engine.replace_line(1, id, 0, at, old, text);
        self.until(|e| match e {
            Event::Edited(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn undo(&mut self) -> EditResult {
        let id = self.id();
        self.engine.undo(1, id);
        self.until(|e| match e {
            Event::Edited(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn save_as(&mut self, path: &Path) {
        let id = self.id();
        self.engine.save_as(1, id, &path.to_string_lossy());
        let (status, message) = self.until(|e| match e {
            Event::Saved { id: got, status, message, .. } if got == id => Some((status, message)),
            _ => None,
        });
        assert_eq!(status, SaveStatus::Done, "{message}");
    }

    fn save(&mut self, path: &Path) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        let (status, message) = self.until(|e| match e {
            Event::Saved { id: got, status, message, .. } if got == id => Some((status, message)),
            _ => None,
        });
        assert_eq!(status, SaveStatus::Done, "{message}");
    }
}

fn have_font() -> bool {
    ["arial.ttf", "LiberationSans-Regular.ttf", "DejaVuSans.ttf"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file() || Path::new("/usr/share/fonts/truetype/dejavu").join(f).is_file() || Path::new("/usr/share/fonts/truetype/liberation").join(f).is_file())
}

fn out_dir(name: &str) -> PathBuf {
    common::fresh_out_dir(&format!("retext-{name}"))
}

/// One page of 300 by 400 points: `page 1` on a baseline 50 points from the top, `quick brown fox` under it.
fn page_file() -> Vec<u8> {
    let text = "BT /Helv 14 Tf 30 350 Td (page 1) Tj 0 -16 Td [(quick) -250 (brown) -250 (fox)] TJ ET";
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let mut put = |out: &mut Vec<u8>, n: u32, body: String| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    put(&mut out, 1, "<< /Type /Catalog /Pages 2 0 R >>".to_string());
    put(&mut out, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] /Resources << /Font << /Helv 5 0 R >> >> >>".to_string());
    put(&mut out, 3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_string());
    put(&mut out, 4, format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    put(&mut out, 5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string());
    let xref_at = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for n in 1..6 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
    out
}

fn text_of(path: &Path) -> String {
    let doc = Document::open(path).unwrap();
    let mut ex = TextExtractor::new(&doc);
    ex.page_text(&doc.pages().unwrap()[0]).unwrap()
}

#[test]
fn a_line_is_asked_for_changed_undone_and_saved() {
    if !have_font() {
        return;
    }
    let dir = out_dir("line");
    let path = dir.join("a.pdf");
    let original = page_file();
    std::fs::write(&path, &original).unwrap();
    let mut v = Viewer::open(&path, "");
    // The line under a click (the second line's baseline is 66 points from the top), and no line where there is no text.
    let (status, _, line) = v.line([40.0, 46.0]);
    assert_eq!(status, TextStatus::Ok);
    let line = line.expect("a line");
    assert_eq!(line.text, "page 1");
    assert!(line.why.is_empty() && line.rect[0] <= 31.0 && line.rect[2] > 55.0 && line.rect[1] < 46.0 && line.rect[3] > 46.0, "{line:?}");
    assert_eq!(v.line([40.0, 62.0]).2.unwrap().text, "quick brown fox");
    assert!(v.line([40.0, 300.0]).2.is_none());
    // Changing it: done, one step to undo, a file that differs from the saved one; what the old text said is not on the page.
    let done = v.replace([40.0, 46.0], "page 1", "page 2");
    assert_eq!(done.status, EditStatus::Done, "{}", done.message);
    assert!(done.can_undo && done.dirty && done.message.is_empty(), "{done:?}");
    assert_eq!(v.line([40.0, 46.0]).2.unwrap().text, "page 2");
    // Wider text says so.
    let wider = v.replace([40.0, 62.0], "quick brown fox", "quick brown fox jumps over the lazy dog");
    assert_eq!(wider.status, EditStatus::Done);
    assert!(wider.message.contains("wider"), "{}", wider.message);
    // A stale request and a bad one are refused and leave the file alone.
    let stale = v.replace([40.0, 46.0], "page 1", "x");
    assert_eq!(stale.status, EditStatus::Failed);
    let bad = v.replace([40.0, 46.0], "page 2", "a\nb");
    assert_eq!(bad.status, EditStatus::Failed);
    assert_eq!(v.undo().status, EditStatus::Done);
    assert_eq!(v.line([40.0, 62.0]).2.unwrap().text, "quick brown fox");
    // Saved: the original bytes first, one update, and the text is the new one.
    let saved = dir.join("saved.pdf");
    v.save(&saved);
    let bytes = std::fs::read(&saved).unwrap();
    assert!(bytes.starts_with(&original) && bytes.len() > original.len());
    assert_eq!(text_of(&saved), "page 2\nquick brown fox\n");
    if let Some(qpdf) = common::find_qpdf() {
        // (An edit that was undone leaves its object numbers unused, which qpdf mentions: a warning, not an error.)
        let check = common::qpdf_check(&qpdf, &saved);
        assert!(check.code == 0 || (check.code == 3 && !check.text.contains("ERROR")), "{}", check.text);
    }
}

#[test]
fn a_file_that_may_not_be_changed_gives_no_line_to_change_and_no_change() {
    if !have_font() {
        return;
    }
    let dir = out_dir("rights");
    let plain = dir.join("plain.pdf");
    std::fs::write(&plain, page_file()).unwrap();
    if let Some(qpdf) = common::find_qpdf() {
        let locked = dir.join("locked.pdf");
        let made = Command::new(&qpdf).args(["--encrypt", "", "ownerpw", "256", "--modify=none", "--", plain.to_str().unwrap(), locked.to_str().unwrap()]).output().unwrap();
        assert!(made.status.success());
        let mut v = Viewer::open(&locked, "");
        let (status, message, line) = v.line([40.0, 46.0]);
        assert_eq!(status, TextStatus::Denied, "{message}");
        assert!(line.is_none());
        assert_eq!(v.replace([40.0, 46.0], "page 1", "page one").status, EditStatus::Denied);
        // With the owner password it is allowed.
        let mut v = Viewer::open(&locked, "ownerpw");
        assert_eq!(v.replace([40.0, 46.0], "page 1", "page one").status, EditStatus::Done);
    }
    // A signed file: refused, and told why.
    let signed = common::public_file("incremental/signature_reason.pdf");
    let mut v = Viewer::open(&signed, "");
    let (status, message, _) = v.line([100.0, 100.0]);
    assert_eq!(status, TextStatus::Failed, "{message}");
    assert!(message.contains("signed") || message.contains("certified"), "{message}");
    let r = v.replace([100.0, 100.0], "x", "y");
    assert_eq!(r.status, EditStatus::Failed);
    assert!(r.message.contains("signed") || r.message.contains("certified"), "{}", r.message);
}

/// The bytes of the file and of every stream in it decoded: all the places the bytes of a text could be.
fn everything(path: &Path) -> Vec<u8> {
    let mut out = std::fs::read(path).unwrap();
    let doc = Document::open(path).unwrap();
    for n in 1..doc.next_free_number() {
        if let Ok(qingpdf_core::Object::Stream(s)) = doc.get(qingpdf_core::ObjRef::new(n, 0)) {
            out.extend(doc.decode_stream(&s).unwrap_or_default());
        }
    }
    out
}

fn has(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn save_as_writes_the_whole_file_again_so_that_the_old_text_is_not_in_it_and_the_document_goes_on_from_there() {
    if !have_font() {
        return;
    }
    let dir = out_dir("saveas");
    let path = dir.join("a.pdf");
    let original = page_file();
    std::fs::write(&path, &original).unwrap();
    let mut v = Viewer::open(&path, "");
    assert_eq!(v.replace([40.0, 62.0], "quick brown fox", "slow green dog").status, EditStatus::Done);
    // Save: the file and an update, and the old text is in the bytes of the file that were there.
    let plain = dir.join("plain.pdf");
    v.save(&plain);
    assert!(std::fs::read(&plain).unwrap().starts_with(&original));
    assert!(has(&everything(&plain), "quick"));
    // Save as: nothing of the old text in the file, in a stream or out of one; the original bytes are not its start.
    let mut w = Viewer::open(&path, "");
    assert_eq!(w.replace([40.0, 62.0], "quick brown fox", "slow green dog").status, EditStatus::Done);
    let fresh = dir.join("fresh.pdf");
    w.save_as(&fresh);
    let all = everything(&fresh);
    assert!(!has(&all, "quick") && !has(&all, "brown") && has(&all, "slow"), "the old text is in the new file");
    assert!(!std::fs::read(&fresh).unwrap().starts_with(&original));
    assert_eq!(text_of(&fresh), "page 1\nslow green dog\n");
    // The document is the new file now: its text, no history to go back through, and a save appends to it.
    assert_eq!(w.line([40.0, 62.0]).2.unwrap().text, "slow green dog");
    assert_eq!(w.undo().status, EditStatus::Nothing);
    assert_eq!(w.replace([40.0, 46.0], "page 1", "page 2").status, EditStatus::Done);
    let again = dir.join("again.pdf");
    w.save(&again);
    assert!(std::fs::read(&again).unwrap().starts_with(&std::fs::read(&fresh).unwrap()));
    assert_eq!(text_of(&again), "page 2\nslow green dog\n");
    // An encrypted file stays encrypted (qpdf says how).
    if let Some(qpdf) = common::find_qpdf() {
        let locked = dir.join("locked.pdf");
        let made = Command::new(&qpdf).args(["--encrypt", "", "ownerpw", "256", "--modify=none", "--", path.to_str().unwrap(), locked.to_str().unwrap()]).output().unwrap();
        assert!(made.status.success());
        let mut x = Viewer::open(&locked, "ownerpw");
        assert_eq!(x.replace([40.0, 62.0], "quick brown fox", "slow green dog").status, EditStatus::Done);
        let saved = dir.join("locked-as.pdf");
        x.save_as(&saved);
        assert!(Document::open(&saved).unwrap().is_encrypted());
        assert!(common::qpdf_encryption(&qpdf, &saved, "ownerpw").is_some());
        let all = everything(&saved);
        assert!(!has(&all, "quick") && has(&all, "slow"));
        let check = common::qpdf_check(&qpdf, &saved);
        assert!(check.code == 0 || (check.code == 3 && !check.text.contains("ERROR")), "{}", check.text);
    }
}
