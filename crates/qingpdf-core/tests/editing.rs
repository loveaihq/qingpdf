//! Step 4a through the engine's messages: annotations added and taken out, undo and redo, saving as an incremental update.
//! Every saved file is checked the same ways: the bytes of the original are an exact prefix, `qpdf --check` accepts it, our own
//! reader opens it again and sees the annotation, and the page is drawn differently only where the annotation is. The other
//! readers (PDFium, MuPDF) are checked by `tests/tools/annot_compare.py`, which draws the files these tests leave in
//! `tests/out/editing`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qingpdf_core::Document;
use qingpdf_core::object::Object;
use qingpdf_core::view::{
    AnnotInfo, EditResult, EditStatus, Engine, Event, MarkupKind, Opened, RenderRequest, Rendered, SaveStatus, TOTAL_BYTES, line_rects,
};

struct Viewer {
    engine: Engine,
    opened: Opened,
    next: u64,
}

impl Viewer {
    fn open(path: &Path, password: &str) -> Viewer {
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &path.to_string_lossy(), password, TOTAL_BYTES, 1920 * 1080);
        match engine.wait(20_000) {
            Some(Event::Opened(opened)) => Viewer { engine, opened, next: 100 },
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

    fn edited(&self, id: u64) -> EditResult {
        self.until(|e| match e {
            Event::Edited(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn boxes(&mut self, page: u32) -> Vec<[f32; 4]> {
        let id = self.id();
        self.engine.char_boxes(1, id, page);
        self.until(|e| match e {
            Event::CharBoxes(b) if b.id == id => Some(b.boxes),
            _ => None,
        })
    }

    fn annotations(&mut self, page: u32) -> Vec<AnnotInfo> {
        let id = self.id();
        self.engine.annotations(1, id, page);
        self.until(|e| match e {
            Event::Annotations { id: i, items, .. } if i == id => Some(items),
            _ => None,
        })
    }

    fn render(&mut self, page: u32) -> Rendered {
        let id = self.id();
        self.engine.render(RenderRequest { doc: 1, id, page, dpi: 72.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
        self.until(|e| match e {
            Event::Rendered(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn markup(&mut self, page: u32, kind: MarkupKind, rects: Vec<[f32; 4]>, color: [u8; 3]) -> EditResult {
        let id = self.id();
        self.engine.add_markup(1, id, page, kind, rects, color, "Tester");
        self.edited(id)
    }

    fn ink(&mut self, page: u32, strokes: Vec<Vec<[f32; 2]>>, color: [u8; 3], width: f32) -> EditResult {
        let id = self.id();
        self.engine.add_ink(1, id, page, strokes, color, width, "Tester");
        self.edited(id)
    }

    fn note(&mut self, page: u32, at: [f32; 2], text: &str, color: [u8; 3]) -> EditResult {
        let id = self.id();
        self.engine.add_note(1, id, page, at, text, color, "Tester");
        self.edited(id)
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

    fn save(&mut self, path: &Path) -> (SaveStatus, String, u64) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, bytes, .. } if i == id => Some((status, message, bytes)),
            _ => None,
        })
    }
}

fn corpus(rel: &str) -> PathBuf {
    common::public_file(rel)
}

/// The first page with at least twenty characters of text, and the lines of its first characters.
fn text_page(v: &mut Viewer) -> (u32, Vec<[f32; 4]>) {
    for page in 0..v.opened.pages.len().min(8) as u32 {
        let boxes = v.boxes(page);
        let real: Vec<[f32; 4]> = boxes.iter().copied().filter(|b| b[2] > b[0] && b[3] > b[1]).collect();
        if real.len() >= 20 {
            let lines = line_rects(&boxes[..boxes.len().min(60)]);
            return (page, lines);
        }
    }
    panic!("no page with text");
}

/// The rectangle of changed pixels between two drawings of a page (`None` if they are the same) and the changed pixel colours.
fn changes(before: &Rendered, after: &Rendered) -> Option<([u32; 4], Vec<[u8; 3]>)> {
    assert_eq!((before.width, before.height), (after.width, after.height));
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut colors = Vec::new();
    for y in 0..before.height {
        for x in 0..before.width {
            let i = ((y * before.width + x) * 4) as usize;
            if before.bgra[i..i + 3] != after.bgra[i..i + 3] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                colors.push([after.bgra[i + 2], after.bgra[i + 1], after.bgra[i]]);
            }
        }
    }
    (x1 >= x0 && y1 >= y0).then_some(([x0, y0, x1 + 1, y1 + 1], colors))
}

fn inside(inner: [u32; 4], outer: [f32; 4], slack: f32) -> bool {
    inner[0] as f32 >= outer[0] - slack && inner[1] as f32 >= outer[1] - slack && inner[2] as f32 <= outer[2] + slack && inner[3] as f32 <= outer[3] + slack
}

fn union(rects: &[[f32; 4]]) -> [f32; 4] {
    rects.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |a, r| [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])])
}

/// What every saved file must satisfy: the original is a prefix, qpdf accepts it (`password` for the encrypted ones), and our reader
/// opens it and counts `annots` annotations on `page`.
fn check_saved(original: &Path, saved: &Path, password: &str, page: usize, annots: usize) {
    let before = std::fs::read(original).unwrap();
    let after = std::fs::read(saved).unwrap();
    assert!(after.len() > before.len(), "nothing was appended");
    assert!(after.starts_with(&before), "{}: the bytes of the original changed", saved.display());
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = if password.is_empty() { common::qpdf_check(&qpdf, saved) } else { common::qpdf_check_pw(&qpdf, saved, password) };
        let original_verdict = if password.is_empty() { common::qpdf_check(&qpdf, original) } else { common::qpdf_check_pw(&qpdf, original, password) };
        // No worse than the original (0 fine, 3 warnings, 2 errors), and not one warning the original did not give as well.
        let rank = |code: i32| match code {
            0 => 0,
            3 => 1,
            _ => 2,
        };
        assert!(rank(verdict.code) <= rank(original_verdict.code), "{}: qpdf --check says {}:\n{}", saved.display(), verdict.code, verdict.text);
        let known = common::qpdf_complaints(&original_verdict.text, original);
        let new: Vec<String> = common::qpdf_complaints(&verdict.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
        assert!(new.is_empty(), "{}: qpdf warns of what the original did not: {new:?}\n{}", saved.display(), verdict.text);
    } else {
        println!("SKIPPED the qpdf check: qpdf was not found");
    }
    let doc = Document::open_with_password(saved, password).unwrap();
    let pages = doc.pages().unwrap();
    let d = doc.get(pages[page].obj_ref).unwrap();
    let Some(Object::Dict(d)) = Some(d) else { panic!("page is not a dictionary") };
    let list = doc.resolve(d.get("Annots").unwrap_or(&Object::Null)).unwrap();
    assert_eq!(list.as_array().map_or(0, <[Object]>::len), annots, "{}: annotations on the page", saved.display());
}

/// How many entries the page's `/Annots` has in the file as it is.
fn annots_on(path: &Path, password: &str, page: usize) -> usize {
    let doc = Document::open_with_password(path, password).unwrap();
    let pages = doc.pages().unwrap();
    match doc.get(pages[page].obj_ref).unwrap().as_dict().and_then(|d| d.get("Annots")) {
        Some(a) => doc.resolve(a).unwrap().as_array().map_or(0, <[Object]>::len),
        None => 0,
    }
}

fn out_dir() -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("editing");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Add one annotation of each kind to a file and check the result every way. `name` names the output files.
fn all_kinds(name: &str, input: &Path, password: &str) {
    let out = out_dir();
    let mut v = Viewer::open(input, password);
    assert!(v.opened.rights.annotate);
    let (page, lines) = text_page(&mut v);
    let size = v.opened.pages[page as usize];
    let first = lines[0];
    let existing = annots_on(input, password, page as usize);
    let base = v.render(page);
    let mut done = existing;
    let mut last_render = base;
    let mut colors_seen: Vec<(&str, [u8; 3])> = Vec::new();

    let marks = [
        (MarkupKind::Highlight, "highlight", [255, 235, 0]),
        (MarkupKind::Underline, "underline", [0, 160, 0]),
        (MarkupKind::StrikeOut, "strikeout", [220, 0, 0]),
        (MarkupKind::Squiggly, "squiggly", [0, 0, 220]),
    ];
    for (n, (kind, label, color)) in marks.into_iter().enumerate() {
        let line = lines[n.min(lines.len() - 1)];
        let r = v.markup(page, kind, vec![line], color);
        assert_eq!(r.status, EditStatus::Done, "{label}: {}", r.message);
        assert_eq!(r.page, page);
        assert!(r.dirty && r.can_undo && !r.can_redo);
        done += 1;
        let drawn = v.render(page);
        let (changed, pixels) = changes(&last_render, &drawn).unwrap_or_else(|| panic!("{name}: the {label} changed nothing"));
        assert!(inside(changed, line, 3.0), "{name}: {label} drew outside its line: {changed:?} vs {line:?}");
        assert!(pixels.len() >= 20, "{name}: {label}: only {} pixels changed", pixels.len());
        last_render = drawn;
        colors_seen.push((label, pixels[pixels.len() / 2]));
    }
    // A drawing of a stroke across the page, and a note.
    let stroke: Vec<[f32; 2]> = (0..=40).map(|i| [60.0 + i as f32 * 3.0, 40.0 + 20.0 * (i as f32 / 4.0).sin()]).collect();
    let r = v.ink(page, vec![stroke.clone()], [255, 0, 255], 3.0);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    done += 1;
    let drawn = v.render(page);
    let (changed, pixels) = changes(&last_render, &drawn).expect("the drawing changed nothing");
    let want = union(&[stroke.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].max(p[0]), a[3].max(p[1])])]);
    assert!(inside(changed, want, 3.0), "ink outside its stroke: {changed:?} vs {want:?}");
    assert!(pixels.iter().any(|p| p[0] > 200 && p[1] < 60 && p[2] > 200), "ink is not magenta");
    last_render = drawn;
    let note_at = [size.width as f32 - 80.0, 120.0];
    let r = v.note(page, note_at, "測試 note 便签：中文输入", [255, 215, 0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    done += 2; // the note and its popup
    let drawn = v.render(page);
    let (changed, _) = changes(&last_render, &drawn).expect("the note changed nothing");
    assert!(inside(changed, [note_at[0], note_at[1], note_at[0] + 20.0, note_at[1] + 20.0], 2.0), "note outside its icon: {changed:?}");

    // The listing sees what was made.
    let list = v.annotations(page);
    let kinds: Vec<&str> = list.iter().map(|a| a.subtype.as_str()).collect();
    for want in ["Highlight", "Underline", "StrikeOut", "Squiggly", "Ink", "Text"] {
        assert!(kinds.contains(&want), "{name}: no {want} in {kinds:?}");
    }
    let note = list.iter().find(|a| a.subtype == "Text").unwrap();
    assert_eq!(note.contents, "測試 note 便签：中文输入");
    assert_eq!(note.author, "Tester");
    let highlight = list.iter().find(|a| a.subtype == "Highlight").unwrap();
    assert_eq!(highlight.parts.len(), 1);
    for (got, want) in highlight.parts[0].iter().zip(first) {
        assert!((got - want).abs() < 1.0, "the highlight is at {:?}, the line at {first:?}", highlight.parts[0]);
    }

    let saved = out.join(format!("{name}.pdf"));
    let (status, message, bytes) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert_eq!(bytes, std::fs::metadata(&saved).unwrap().len());
    check_saved(input, &saved, password, page as usize, done);

    // Our reader opens the saved file again and draws the same page the same way.
    let mut again = Viewer::open(&saved, password);
    let reread = again.render(page);
    let current = v.render(page);
    assert_eq!(changes(&current, &reread), None, "{name}: the saved file is drawn differently");
    assert_eq!(again.annotations(page).len(), list.len());
    println!("{name}: {} bytes appended, colours {colors_seen:?}", std::fs::metadata(&saved).unwrap().len() - std::fs::metadata(input).unwrap().len());
}

#[test]
fn every_kind_of_annotation_on_a_classic_file() {
    all_kinds("classic", &corpus("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf"), "");
}

#[test]
fn every_kind_of_annotation_on_a_file_with_a_cross_reference_stream() {
    all_kinds("xref-stream", &corpus("zh/gongwen/gongwen-2024-nicheng-work-report.pdf"), "");
}

#[test]
fn every_kind_of_annotation_on_a_hybrid_file() {
    all_kinds("hybrid", &corpus("zh/lunwen/lunwen-arxiv-2403.14268-word-tc.pdf"), "");
}

#[test]
fn the_new_section_follows_the_kind_of_the_old_one() {
    // classic -> table, stream -> stream, hybrid -> table (7.5.8.4).
    for (name, stream) in [("classic", false), ("xref-stream", true), ("hybrid", false)] {
        let saved = std::fs::read(out_dir().join(format!("{name}.pdf"))).unwrap_or_default();
        if saved.is_empty() {
            continue;
        }
        let at = saved.windows(9).rposition(|w| w == b"startxref").unwrap();
        let tail = String::from_utf8_lossy(&saved[at..]).into_owned();
        let offset: usize = tail.lines().nth(1).unwrap().trim().parse().unwrap();
        let section = &saved[offset..offset + 20];
        assert_eq!(section.starts_with(b"xref"), !stream, "{name}: {:?}", String::from_utf8_lossy(section));
    }
}

#[test]
fn encrypted_files_keep_their_encryption_and_the_new_parts_are_encrypted_with_the_key() {
    let dir = "encrypted/qpdf-generated/";
    // (file, password): RC4 128 bits, AES-128 and AES-256 (revisions 3, 4 and 6; also revision 5).
    for (file, password) in [
        ("utf8.r3-128-rc4-empty-print-none.pdf", ""),
        ("bookmarks.r3-128-rc4-user-modify-none.pdf", "owner"),
        ("pdf20utf8.r4-aes128-empty-modify-none.pdf", "owner"),
        ("bookmarks.r4-aes128-user-assemble-n.pdf", "user"),
        ("vertical.r4-aes128-user-chinese.pdf", "密码"),
        ("bookmarks.r5-aes256-empty-print-none.pdf", ""),
        ("pdf20utf8.r6-aes256-user-modify-none.pdf", "owner"),
        ("vertical.r6-aes256-user-chinese.pdf", "密码"),
        ("objstm.r6-aes256-empty-extract-n.pdf", ""),
        ("objstm.r4-aes128-empty-print-none.pdf", ""),
    ] {
        let input = corpus(&format!("{dir}{file}"));
        let mut v = Viewer::open(&input, password);
        let page = 0;
        let r = v.note(page, [100.0, 100.0], "secret note 机密", [255, 215, 0]);
        assert_eq!(r.status, EditStatus::Done, "{file}: {}", r.message);
        let r = v.ink(page, vec![vec![[50.0, 50.0], [120.0, 90.0], [200.0, 60.0]]], [255, 0, 0], 2.0);
        assert_eq!(r.status, EditStatus::Done, "{file}: {}", r.message);
        let saved = out_dir().join(format!("enc-{file}"));
        assert_eq!(v.save(&saved).0, SaveStatus::Done);
        let original_pages = Document::open_with_password(&input, password).unwrap().pages().unwrap().len();
        // The note, its popup and the drawing.
        check_saved_encrypted(&input, &saved, password, page as usize, annots_on(&input, password, 0) + 3, original_pages);
    }
}

fn check_saved_encrypted(input: &Path, saved: &Path, password: &str, page: usize, annots: usize, pages: usize) {
    let before = std::fs::read(input).unwrap();
    let after = std::fs::read(saved).unwrap();
    assert!(after.starts_with(&before), "{}: the original changed", saved.display());
    let doc = Document::open_with_password(saved, password).unwrap();
    assert!(doc.is_encrypted());
    assert_eq!(doc.pages().unwrap().len(), pages);
    // The note's text and its appearance stream decrypt with the file's key.
    let p = doc.pages().unwrap();
    let d = doc.get(p[page].obj_ref).unwrap();
    let list = doc.resolve(d.as_dict().unwrap().get("Annots").unwrap()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), annots);
    let mut text_found = false;
    for item in list.as_array().unwrap() {
        let a = doc.resolve(item).unwrap();
        let a = a.as_dict().unwrap();
        if a.get_name("Subtype").is_some_and(|n| n == "Text") {
            let Some(Object::String(s)) = a.get("Contents") else { panic!("note without text") };
            assert_eq!(qingpdf_core::info::decode_text_string(&s.bytes), "secret note 机密");
            text_found = true;
            let ap = doc.resolve(a.get("AP").unwrap()).unwrap();
            let n = doc.resolve(ap.as_dict().unwrap().get("N").unwrap()).unwrap();
            let Object::Stream(stream) = n else { panic!("no appearance stream") };
            let content = doc.decode_stream(&stream).unwrap();
            assert!(String::from_utf8_lossy(&content).contains(" m "), "{}", String::from_utf8_lossy(&content));
        }
    }
    assert!(text_found);
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = common::qpdf_check_pw(&qpdf, saved, password);
        assert!(verdict.code == 0 || verdict.code == 3, "{}: {}", saved.display(), verdict.text);
        let original = common::qpdf_check_pw(&qpdf, input, password);
        let known = common::qpdf_complaints(&original.text, input);
        let new: Vec<String> = common::qpdf_complaints(&verdict.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
        assert!(new.is_empty(), "{}: qpdf warns of what the original did not: {new:?}", saved.display());
        // qpdf decrypts it with the password and finds the annotation text.
        let decrypted = out_dir().join("qpdf-decrypted.pdf");
        let run = common::qpdf_with_password(&qpdf, password, &["--decrypt".as_ref(), saved.as_os_str(), decrypted.as_os_str()]);
        assert!(matches!(run.status.code(), Some(0 | 3)), "{}", String::from_utf8_lossy(&run.stderr));
        let plain = std::fs::read(&decrypted).unwrap();
        assert!(common::contains(&plain, b"/Subtype /Text") || common::contains(&plain, b"/Subtype/Text"));
        let _ = std::fs::remove_file(&decrypted);
    }
}

#[test]
fn a_file_that_forbids_annotations_refuses_without_the_owner_password_and_allows_with_it() {
    let input = corpus("encrypted/qpdf-generated/two.r6-aes256-user-everything-denied.pdf");
    let mut v = Viewer::open(&input, "user");
    assert!(!v.opened.rights.annotate);
    let r = v.note(0, [50.0, 50.0], "no", [255, 215, 0]);
    assert_eq!((r.status, r.dirty), (EditStatus::Denied, false), "{}", r.message);
    let r = v.ink(0, vec![vec![[10.0, 10.0], [50.0, 50.0]]], [0, 0, 0], 2.0);
    assert_eq!(r.status, EditStatus::Denied);
    let r = v.markup(0, MarkupKind::Highlight, vec![[10.0, 10.0, 80.0, 30.0]], [255, 255, 0]);
    assert_eq!(r.status, EditStatus::Denied);
    assert_eq!(v.undo().status, EditStatus::Nothing);
    drop(v);
    let mut v = Viewer::open(&input, "owner");
    assert!(v.opened.rights.annotate);
    let r = v.note(0, [50.0, 50.0], "yes", [255, 215, 0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let saved = out_dir().join("denied-then-owner.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let again = Document::open_with_password(&saved, "owner").unwrap();
    assert!(again.is_encrypted());
}

#[test]
fn undo_and_redo_cut_off_and_put_back_the_appended_bytes() {
    let input = corpus("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf");
    let original = std::fs::read(&input).unwrap();
    let mut v = Viewer::open(&input, "");
    let (page, lines) = text_page(&mut v);
    let blank = v.render(page);
    assert_eq!(v.undo().status, EditStatus::Nothing);
    let a = v.markup(page, MarkupKind::Highlight, vec![lines[0]], [255, 235, 0]);
    assert!(a.dirty && a.can_undo && !a.can_redo);
    let marked = v.render(page);
    assert!(changes(&blank, &marked).is_some());
    let b = v.ink(page, vec![vec![[60.0, 60.0], [150.0, 100.0]]], [255, 0, 0], 2.0);
    assert_eq!(b.status, EditStatus::Done);
    let both = v.render(page);
    // Undo twice: the page is as it was and nothing is changed.
    let u = v.undo();
    assert_eq!((u.status, u.page, u.can_undo, u.can_redo, u.dirty), (EditStatus::Done, page, true, true, true));
    assert_eq!(changes(&marked, &v.render(page)), None, "undoing the drawing did not give the page with only the highlight");
    assert!(changes(&both, &v.render(page)).is_some(), "the drawing was not taken away");
    let u = v.undo();
    assert_eq!((u.can_undo, u.can_redo, u.dirty), (false, true, false));
    assert_eq!(changes(&blank, &v.render(page)), None);
    // Redo twice: the page is as it was after the second edit.
    assert_eq!(v.redo().status, EditStatus::Done);
    let r = v.redo();
    assert_eq!((r.can_undo, r.can_redo, r.dirty), (true, false, true));
    assert_eq!(changes(&both, &v.render(page)), None);
    assert_eq!(v.redo().status, EditStatus::Nothing);
    // A new edit after an undo forgets what could be redone.
    v.undo();
    let n = v.note(page, [100.0, 100.0], "after undo", [255, 215, 0]);
    assert!(!n.can_redo);
    // Saved, and then not dirty; undone after saving, dirty again.
    let saved = out_dir().join("undo.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let after = std::fs::read(&saved).unwrap();
    assert!(after.starts_with(&original));
    let u = v.undo();
    assert!(u.dirty);
    let r = v.redo();
    assert!(!r.dirty);
}

#[test]
fn an_annotation_can_be_taken_out_again_and_so_can_one_that_was_there_before() {
    let input = corpus("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf");
    let mut v = Viewer::open(&input, "");
    let (page, lines) = text_page(&mut v);
    let blank = v.render(page);
    v.note(page, [100.0, 100.0], "to go", [255, 215, 0]);
    v.markup(page, MarkupKind::Underline, vec![lines[0]], [0, 0, 255]);
    let list = v.annotations(page);
    let note = list.iter().find(|a| a.subtype == "Text").unwrap().clone();
    let id = v.id();
    v.engine.delete_annotation(1, id, page, note.index, note.num);
    let r = v.edited(id);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let after = v.annotations(page);
    // The note and its popup are gone; the underline is still there.
    assert!(after.iter().all(|a| a.subtype != "Text") && after.iter().any(|a| a.subtype == "Underline"));
    // Undoing the removal brings the note back, in the list and on the page.
    let gone = v.render(page);
    assert_eq!(v.undo().status, EditStatus::Done);
    assert!(v.annotations(page).iter().any(|a| a.subtype == "Text"), "the note did not come back");
    assert!(changes(&gone, &v.render(page)).is_some(), "the page is drawn without the note after the removal was undone");
    assert_eq!(v.redo().status, EditStatus::Done);
    assert!(v.annotations(page).iter().all(|a| a.subtype != "Text"));
    let saved = out_dir().join("deleted.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    // Delete the underline from the saved file: an annotation made by an earlier session.
    let mut w = Viewer::open(&saved, "");
    let list = w.annotations(page);
    let underline = list.iter().find(|a| a.subtype == "Underline").unwrap().clone();
    let id = w.id();
    w.engine.delete_annotation(1, id, page, underline.index, underline.num);
    assert_eq!(w.edited(id).status, EditStatus::Done);
    assert_eq!(changes(&blank, &w.render(page)), None, "the page is not as it was before all the annotations");
    // A stale request is refused.
    let id = w.id();
    w.engine.delete_annotation(1, id, page, underline.index, underline.num);
    assert_eq!(w.edited(id).status, EditStatus::Failed);
}

/// A file of `n` pages whose page `i` says "page i".
fn many_pages(n: usize) -> Vec<u8> {
    pages_with(n, "/MediaBox [0 0 612 792]")
}

/// [`many_pages`] with `page_entries` (the boxes, the turn) in every page dictionary.
fn pages_with(n: usize, page_entries: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0usize; 4 + 2 * n];
    let mut put = |out: &mut Vec<u8>, num: usize, body: String| {
        offsets[num] = out.len();
        out.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    put(&mut out, 1, "<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    put(&mut out, 2, format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")));
    put(&mut out, 3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
    for i in 0..n {
        put(&mut out, 4 + 2 * i, format!("<< /Type /Page /Parent 2 0 R {page_entries} /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + 2 * i));
        let content = format!("BT /F1 24 Tf 72 700 Td (Page {i} of the test file with some words) Tj ET");
        put(&mut out, 5 + 2 * i, format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", 3 + 2 * n + 1).as_bytes());
    for off in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", 3 + 2 * n + 1).as_bytes());
    out
}

#[test]
fn saving_one_annotation_on_a_hundred_page_file_takes_under_200_milliseconds() {
    let dir = out_dir();
    let input = dir.join("hundred.pdf");
    std::fs::write(&input, many_pages(100)).unwrap();
    let mut v = Viewer::open(&input, "");
    assert_eq!(v.opened.pages.len(), 100);
    let boxes = v.boxes(50);
    let line = line_rects(&boxes[..boxes.len().min(20)])[0];
    // From asking for the markup to the file being on the disk.
    let saved = dir.join("hundred-saved.pdf");
    let started = Instant::now();
    let r = v.markup(50, MarkupKind::Highlight, vec![line], [255, 235, 0]);
    assert_eq!(r.status, EditStatus::Done);
    let edit_time = started.elapsed();
    let saving = Instant::now();
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let save_time = saving.elapsed();
    println!("one annotation on 100 pages: edit {edit_time:?}, save {save_time:?}, together {:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_millis(200), "edit + save took {:?}", started.elapsed());
    check_saved(&input, &saved, "", 50, 1);
}

#[test]
fn a_file_with_a_damaged_cross_reference_table_is_written_whole() {
    let dir = out_dir();
    // The page file above with its table cut off: it is rebuilt by scanning.
    let mut bytes = many_pages(5);
    let at = bytes.windows(4).rposition(|w| w == b"xref").unwrap();
    bytes.truncate(at);
    bytes.extend_from_slice(b"startxref\n99999\n%%EOF\n");
    let input = dir.join("damaged.pdf");
    std::fs::write(&input, &bytes).unwrap();
    let mut v = Viewer::open(&input, "");
    let boxes = v.boxes(1);
    let line = line_rects(&boxes[..boxes.len().min(20)])[0];
    let r = v.markup(1, MarkupKind::Highlight, vec![line], [255, 235, 0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    assert!(r.rewrote, "the file was not said to be written afresh");
    assert_eq!(v.annotations(1).len(), 1);
    let saved = dir.join("damaged-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let doc = Document::open(&saved).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(doc.pages().unwrap().len(), 5);
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = common::qpdf_check(&qpdf, &saved);
        assert_eq!(verdict.code, 0, "{}", verdict.text);
    }
    // The original is untouched on the disk.
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}

#[test]
fn limits_hold_for_points_notes_and_the_number_of_strokes() {
    let input = corpus("xref-classic/hello_world.pdf");
    let mut v = Viewer::open(&input, "");
    // 100,000 points are thinned to the limit; a wild coordinate is refused.
    let wild: Vec<[f32; 2]> = (0..100_000).map(|i| [(i % 500) as f32, (i % 300) as f32]).collect();
    let r = v.ink(0, vec![wild], [0, 0, 0], 1.0);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let r = v.ink(0, vec![vec![[f32::NAN, 1.0], [2.0, 2.0]]], [0, 0, 0], 1.0);
    assert_eq!(r.status, EditStatus::Failed);
    let r = v.ink(0, vec![vec![[1.0, 1.0]]; 65], [0, 0, 0], 1.0);
    assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    let long = "字".repeat(10_000);
    let r = v.note(0, [10.0, 10.0], &long, [255, 215, 0]);
    assert_eq!(r.status, EditStatus::Done);
    let notes = v.annotations(0);
    let note = notes.iter().find(|a| a.subtype == "Text").unwrap();
    assert!(note.contents.chars().count() <= qingpdf_core::view::MAX_CONTENTS_CHARS);
    let saved = out_dir().join("limits.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let doc = Document::open(&saved).unwrap();
    let page = &doc.pages().unwrap()[0];
    let d = doc.get(page.obj_ref).unwrap();
    let list = doc.resolve(d.as_dict().unwrap().get("Annots").unwrap()).unwrap();
    let ink = doc.resolve(&list.as_array().unwrap()[0]).unwrap();
    let Some(Object::Array(strokes)) = ink.as_dict().unwrap().get("InkList") else { panic!("no ink list") };
    let Object::Array(points) = &strokes[0] else { panic!("stroke") };
    assert!(points.len() / 2 <= qingpdf_core::edit::MAX_INK_POINTS, "{} points", points.len() / 2);
    let text = doc.resolve(&list.as_array().unwrap()[1]).unwrap();
    let Some(Object::String(contents)) = text.as_dict().unwrap().get("Contents") else { panic!("no contents") };
    assert_eq!(qingpdf_core::info::decode_text_string(&contents.bytes).chars().count(), qingpdf_core::edit::MAX_NOTE_CHARS);
}

#[test]
fn a_note_and_a_drawing_can_be_added_to_every_file_of_the_corpus_that_opens() {
    let out = common::fresh_out_dir("editing-corpus");
    let qpdf = common::find_qpdf();
    let (mut done, mut skipped, mut rewritten, mut quirks) = (0usize, 0usize, 0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();
    for (n, input) in common::corpus_files().iter().enumerate() {
        let name = common::corpus_name(input);
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &input.to_string_lossy(), "", TOTAL_BYTES, 1920 * 1080);
        let opened = match engine.wait(60_000) {
            Some(Event::Opened(o)) => o,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let mut v = Viewer { engine, opened, next: 100 };
        if !v.opened.rights.annotate || v.opened.pages.is_empty() {
            skipped += 1;
            continue;
        }
        let r = v.note(0, [30.0, 30.0], "corpus note", [255, 215, 0]);
        if r.status != EditStatus::Done {
            failures.push(format!("{name}: note: {:?} {}", r.status, r.message));
            continue;
        }
        let r = v.ink(0, vec![vec![[40.0, 60.0], [80.0, 90.0], [120.0, 60.0]]], [255, 0, 0], 2.0);
        if r.status != EditStatus::Done {
            failures.push(format!("{name}: ink: {:?} {}", r.status, r.message));
            continue;
        }
        let saved = out.join(format!("{n:03}.pdf"));
        let (status, message, _) = v.save(&saved);
        if status != SaveStatus::Done {
            failures.push(format!("{name}: save: {message}"));
            continue;
        }
        let original = std::fs::read(input).unwrap();
        let after = std::fs::read(&saved).unwrap();
        let was_rewritten = r.rewrote;
        if was_rewritten {
            rewritten += 1;
        } else if !after.starts_with(&original) {
            failures.push(format!("{name}: the bytes of the original changed"));
            continue;
        }
        // Our own reader opens it again and lists the two annotations on the page.
        let mut again = Viewer::open(&saved, "");
        let kinds: Vec<String> = again.annotations(0).into_iter().map(|a| a.subtype).collect();
        if !(kinds.iter().any(|k| k == "Text") && kinds.iter().any(|k| k == "Ink")) {
            failures.push(format!("{name}: the reopened file lists {kinds:?}"));
            continue;
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
    println!("{done} files edited and saved ({rewritten} written whole because their structure was damaged, {quirks} with only the input's own qpdf warnings), {skipped} skipped (locked, no permission, unopenable)");
    for f in &failures {
        println!("FAIL: {f}");
    }
    assert!(failures.is_empty(), "{} file(s) failed", failures.len());
    assert!(done > 50, "only {done} files were edited");
}

#[test]
fn marks_on_a_turned_page_are_drawn_where_they_were_made() {
    // 300 x 200 points turned 90 degrees clockwise: shown 200 wide and 300 high.
    for (turn, name) in [(0, "r0"), (90, "r90"), (180, "r180"), (270, "r270")] {
        let input = out_dir().join(format!("{name}.pdf"));
        std::fs::write(&input, pages_with(1, &format!("/MediaBox [0 0 300 200] /Rotate {turn}"))).unwrap();
        let mut v = Viewer::open(&input, "");
        let size = v.opened.pages[0];
        let before = v.render(0);
        assert_eq!((before.width, before.height), (size.width as u32, size.height as u32));
        let rect = [20.0, 30.0, 90.0, 50.0];
        assert_eq!(v.markup(0, MarkupKind::Highlight, vec![rect], [255, 0, 0]).status, EditStatus::Done);
        let after = v.render(0);
        let (changed, pixels) = changes(&before, &after).expect("the highlight is not on the page");
        assert!(inside(changed, rect, 1.5) && changed[2] - changed[0] >= 66 && changed[3] - changed[1] >= 18, "turn {turn}: {changed:?} is not {rect:?}");
        assert!(pixels.iter().all(|p| p[0] > 200 && p[1] < 80 && p[2] < 80) , "turn {turn}: not red");
        // A stroke and a note on the same page.
        assert_eq!(v.ink(0, vec![vec![[100.0, 100.0], [150.0, 100.0]]], [0, 0, 255], 4.0).status, EditStatus::Done);
        let with_ink = v.render(0);
        assert_eq!(v.note(0, [30.0, 120.0], "n", [255, 215, 0]).status, EditStatus::Done);
        let (icon, _) = changes(&with_ink, &v.render(0)).expect("the note is not on the page");
        assert!(inside(icon, [30.0, 120.0, 50.0, 140.0], 1.5), "turn {turn}: the note icon is at {icon:?}, not at its place");
        let list = v.annotations(0);
        let ink = list.iter().find(|a| a.subtype == "Ink").unwrap();
        assert!(inside([ink.rect[0] as u32 + 2, ink.rect[1] as u32 + 2, ink.rect[2] as u32 - 2, ink.rect[3] as u32 - 2], [98.0, 98.0, 152.0, 102.0], 2.5), "{:?}", ink.rect);
        let note = list.iter().find(|a| a.subtype == "Text").unwrap();
        assert!((note.rect[0] - 30.0).abs() < 0.01 && (note.rect[1] - 120.0).abs() < 0.01 && (note.rect[2] - 50.0).abs() < 0.01, "{:?}", note.rect);
        // (The other readers draw these files in tests/tools/annot_compare.py.)
        assert_eq!(v.save(&out_dir().join(format!("{name}-saved.pdf"))).0, SaveStatus::Done);
    }
}

// --- the review of 4a: files made by hand, through the engine's messages ---------------------------------------------

/// A classic file of the objects given (number, body), written to the test output folder as `name`.
fn hand_made(name: &str, objs: &[(u32, &str)]) -> PathBuf {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objs {
        offsets.insert(*n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let max = objs.iter().map(|o| o.0).max().unwrap();
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max + 1).as_bytes());
    for n in 1..=max {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", max + 1).as_bytes());
    let path = out_dir().join(name);
    std::fs::write(&path, out).unwrap();
    path
}

impl Viewer {
    fn delete(&mut self, page: u32, index: u32, num: u32) -> EditResult {
        let id = self.id();
        self.engine.delete_annotation(1, id, page, index, num);
        self.edited(id)
    }
}

fn subtypes_on(v: &mut Viewer, page: u32) -> Vec<String> {
    v.annotations(page).into_iter().map(|a| a.subtype).collect()
}

#[test]
fn an_array_that_two_pages_share_changes_for_the_page_edited_only() {
    let path = hand_made(
        "shared-array.pdf",
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Annots 5 0 R >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Annots 5 0 R >>"),
            (5, "[ 6 0 R ]"),
            (6, "<< /Type /Annot /Subtype /Square /Rect [10 10 40 40] /C [1 0 0] >>"),
        ],
    );
    let mut v = Viewer::open(&path, "");
    assert_eq!(v.markup(0, MarkupKind::Highlight, vec![[10.0, 10.0, 80.0, 24.0]], [255, 255, 0]).status, EditStatus::Done);
    assert_eq!((subtypes_on(&mut v, 0), subtypes_on(&mut v, 1)), (vec!["Square".to_string(), "Highlight".to_string()], vec!["Square".to_string()]));
    // The shared array was not written again: page 0 has a copy of its own, page 1 still points to the array as it was.
    let probe = out_dir().join("shared-array-probe.pdf");
    assert_eq!(v.save(&probe).0, SaveStatus::Done);
    let doc = Document::open(&probe).unwrap();
    assert_eq!(doc.get(qingpdf_core::object::ObjRef::new(5, 0)).unwrap().as_array().map(<[Object]>::len), Some(1));
    assert_eq!((annots_on(&probe, "", 0), annots_on(&probe, "", 1)), (2, 1));
    // Taking the square off page 1 leaves it on page 0.
    assert_eq!(v.delete(1, 0, 6).status, EditStatus::Done);
    assert_eq!((subtypes_on(&mut v, 0).len(), subtypes_on(&mut v, 1).len()), (2, 0));
    let saved = out_dir().join("shared-array-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&path, &saved, "", 0, 2);
    assert_eq!(annots_on(&saved, "", 1), 0);
}

#[test]
fn a_popup_that_names_another_note_stays_and_replies_and_locks_are_kept() {
    let path = hand_made(
        "popups-replies.pdf",
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Annots [6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R] >>"),
            (6, "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (A) /Popup 8 0 R >>"),
            (7, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (B) /Popup 8 0 R >>"),
            (8, "<< /Type /Annot /Subtype /Popup /Rect [80 50 180 90] /Parent 7 0 R >>"),
            (9, "<< /Type /Annot /Subtype /Square /Rect [100 100 120 120] /Contents (locked) /F 128 >>"),
            (10, "<< /Type /Annot /Subtype /Text /Rect [100 10 120 30] /Contents (root) >>"),
            (11, "<< /Type /Annot /Subtype /Text /Rect [100 10 120 30] /Contents (reply) /IRT 10 0 R >>"),
            (12, "<< /Type /Annot /Subtype /Text /Rect [100 10 120 30] /Contents (reply to the reply) /IRT 11 0 R >>"),
            (13, "<< /Type /Annot /Subtype /Square /Rect [150 150 190 190] >>"),
        ],
    );
    let mut v = Viewer::open(&path, "");
    // Taking A out leaves B its popup, which says its parent is B (and A names it as its own).
    let a = v.annotations(0).iter().find(|a| a.num == 6).unwrap().clone();
    assert_eq!(v.delete(0, a.index, 6).status, EditStatus::Done);
    assert!(v.annotations(0).iter().any(|a| a.num == 7), "B is still there");
    let probe = out_dir().join("popups-probe.pdf");
    assert_eq!(v.save(&probe).0, SaveStatus::Done);
    assert_eq!(annots_on(&probe, "", 0), 7, "A is gone and the popup of B is not");
    // The locked square stays.
    let locked = v.annotations(0).iter().find(|a| a.num == 9).unwrap().clone();
    let r = v.delete(0, locked.index, 9);
    assert_eq!(r.status, EditStatus::Failed, "{}", r.message);
    assert!(r.message.contains("locked"), "{}", r.message);
    // The note with a reply and a reply to the reply: all three go.
    let list = v.annotations(0);
    let root = list.iter().find(|a| a.num == 10).unwrap().clone();
    assert_eq!(v.delete(0, root.index, 10).status, EditStatus::Done);
    let nums: Vec<u32> = v.annotations(0).iter().map(|a| a.num).collect();
    assert_eq!(nums, vec![7, 9, 13], "{nums:?}");
    // Undo brings all three back.
    assert_eq!(v.undo().status, EditStatus::Done);
    assert_eq!(v.annotations(0).len(), 6);
}

/// A one-page file with a catalog that says `catalog_extra` and the objects `extra`.
fn signed_file(name: &str, catalog_extra: &str, extra: &[(u32, &str)]) -> PathBuf {
    let catalog = format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>");
    let mut objs: Vec<(u32, &str)> = vec![
        (1, &catalog),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> >>"),
    ];
    objs.extend_from_slice(extra);
    hand_made(name, &objs)
}

#[test]
fn signed_files_are_edited_as_far_as_their_signatures_allow() {
    const SIG: &str = "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 0 0 0] /Contents <00> >>";
    let certified = |p: u32| format!("<< /Type /Sig /Filter /Adobe.PPKLite /Reference [ << /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P {p} /V /1.2 >> >> ] /Contents <00> >>");
    let note = |v: &mut Viewer| v.note(0, [20.0, 20.0], "n", [255, 215, 0]);
    // P = 1 and P = 2: refused, nothing changed.
    for p in [1, 2] {
        let sig = certified(p);
        let path = signed_file(&format!("certified-{p}.pdf"), "/Perms << /DocMDP 5 0 R >>", &[(5, &sig)]);
        let mut v = Viewer::open(&path, "");
        let r = note(&mut v);
        assert_eq!((r.status, r.dirty, r.can_undo), (EditStatus::Failed, false, false), "P={p}: {}", r.message);
        assert!(r.message.contains("certified"), "{}", r.message);
        assert_eq!(v.ink(0, vec![vec![[1.0, 1.0], [30.0, 30.0]]], [0; 3], 1.0).status, EditStatus::Failed);
    }
    // P = 3: annotations are allowed; the person is told once that the signature will show the change.
    let sig = certified(3);
    let path = signed_file("certified-3.pdf", "/Perms << /DocMDP 5 0 R >>", &[(5, &sig)]);
    let mut v = Viewer::open(&path, "");
    let first = note(&mut v);
    assert_eq!(first.status, EditStatus::Done, "{}", first.message);
    assert!(first.message.contains("signed"), "{}", first.message);
    let second = v.ink(0, vec![vec![[1.0, 1.0], [30.0, 30.0]]], [0; 3], 1.0);
    assert!(second.status == EditStatus::Done && second.message.is_empty(), "told twice: {}", second.message);
    // A signature in a field and no certification: the same.
    let field = "<< /FT /Sig /T (Sig1) /V 6 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 >>";
    let path = signed_file("signed.pdf", "/AcroForm << /Fields [5 0 R] /SigFlags 3 >>", &[(5, field), (6, SIG)]);
    let mut v = Viewer::open(&path, "");
    let first = v.markup(0, MarkupKind::Highlight, vec![[10.0, 10.0, 80.0, 24.0]], [255, 255, 0]);
    assert!(first.status == EditStatus::Done && first.message.contains("signed"), "{first:?}");
    let saved = out_dir().join("signed-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    // The signed bytes are the beginning of the saved file.
    assert!(std::fs::read(&saved).unwrap().starts_with(&std::fs::read(&path).unwrap()));
    // An empty signature field is not a signature.
    let empty = "<< /FT /Sig /T (Sig1) /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 >>";
    let path = signed_file("unsigned-field.pdf", "/AcroForm << /Fields [5 0 R] >>", &[(5, empty)]);
    let mut v = Viewer::open(&path, "");
    let r = note(&mut v);
    assert!(r.status == EditStatus::Done && r.message.is_empty(), "{r:?}");
}

#[test]
fn a_signed_file_with_damaged_structure_is_not_written_again() {
    const SIG: &str = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 0 0 0] /Contents <00> >>";
    let field = "<< /FT /Sig /T (Sig1) /V 6 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 >>";
    let path = signed_file("signed-damaged.pdf", "/AcroForm << /Fields [5 0 R] >>", &[(5, field), (6, SIG)]);
    // Its cross-reference table cut off: it is rebuilt by scanning, and adding to it would need it written afresh.
    let mut bytes = std::fs::read(&path).unwrap();
    let at = bytes.windows(4).rposition(|w| w == b"xref").unwrap();
    bytes.truncate(at);
    bytes.extend_from_slice(b"startxref\n99999\n%%EOF\n");
    std::fs::write(&path, &bytes).unwrap();
    let mut v = Viewer::open(&path, "");
    let r = v.note(0, [20.0, 20.0], "n", [255, 215, 0]);
    assert_eq!((r.status, r.dirty, r.rewrote), (EditStatus::Failed, false, false), "{}", r.message);
    assert!(r.message.contains("signature"), "{}", r.message);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn text_can_be_marked_in_a_file_that_forbids_copying_but_allows_annotating() {
    // R2, no modifying or extracting, but the annotation bit is on.
    let input = corpus("encrypted/qpdf-generated/two.r2-40-empty-modify-extract-none.pdf");
    let mut v = Viewer::open(&input, "");
    assert!(!v.opened.rights.copy && v.opened.rights.annotate);
    let id = v.id();
    v.engine.char_boxes(1, id, 0);
    let boxes = v.until(|e| match e {
        Event::CharBoxes(b) if b.id == id => Some(b),
        _ => None,
    });
    assert_eq!(boxes.status, qingpdf_core::view::TextStatus::Ok, "{}", boxes.message);
    let lines = line_rects(&boxes.boxes);
    assert!(!lines.is_empty());
    let r = v.markup(0, MarkupKind::Highlight, vec![lines[0]], [255, 235, 0]);
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    // Copying stays refused.
    let id = v.id();
    v.engine.copy_text(1, id, (0, 0), (0, 20));
    let (status, text) = v.until(|e| match e {
        Event::Copied { id: i, status, text, .. } if i == id => Some((status, text)),
        _ => None,
    });
    assert_eq!((status, text.as_str()), (qingpdf_core::view::TextStatus::Denied, ""));
    let saved = out_dir().join("marked-no-copy.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    assert_eq!(annots_on(&saved, "", 0), annots_on(&input, "", 0) + 1);
}

#[test]
fn a_thousand_edits_on_a_small_file_grow_it_by_what_they_add_and_a_second_save_adds_to_the_first() {
    let dir = out_dir();
    let input = dir.join("small.pdf");
    std::fs::write(&input, many_pages(1)).unwrap();
    let before = std::fs::metadata(&input).unwrap().len();
    let mut v = Viewer::open(&input, "");
    let boxes = v.boxes(0);
    let line = line_rects(&boxes[..boxes.len().min(20)])[0];
    let started = Instant::now();
    let mut ids = Vec::new();
    for i in 0..1000 {
        let id = v.id();
        if i % 2 == 0 {
            v.engine.add_note(1, id, 0, [(i % 500) as f32, 100.0], &format!("note {i}"), [255, 215, 0], "Tester");
        } else {
            v.engine.add_markup(1, id, 0, MarkupKind::Highlight, vec![line], [255, 235, 0], "Tester");
        }
        ids.push(id);
    }
    for id in ids {
        let r = v.edited(id);
        assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    }
    let edits_time = started.elapsed();
    let saved = dir.join("small-saved.pdf");
    let saving = Instant::now();
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let save_time = saving.elapsed();
    let after = std::fs::metadata(&saved).unwrap().len();
    println!("1000 edits on a {before}-byte file: edits {edits_time:?}, save {save_time:?}, file {after} bytes (+{} KB)", (after - before) / 1024);
    // 500 notes (with popups) and 500 highlights are 1,500 annotations, each a few hundred bytes: well under 1.5 MB, where one
    // update for each edit was 7 MB.
    assert!(after - before < 1_500_000, "the file grew by {} bytes", after - before);
    check_saved(&input, &saved, "", 0, 1500);
    // One cross-reference section more than the file had, not a thousand.
    let bytes = std::fs::read(&saved).unwrap();
    assert_eq!(bytes.windows(9).filter(|w| *w == b"startxref").count(), 2);
}
