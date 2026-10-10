//! The reader's editing state (4a): what can be undone and redone, whether the file differs from the last one saved, what the
//! file's signatures allow, and the password to open the file again with.
//!
//! The edits themselves are objects put in front of the document's own, in memory (`Document::overlay_set`, `edit::commit`); the
//! file is not touched until it is saved, when they are written as one incremental update (`edit::incremental`) and the saved
//! file becomes the document's file. A step of the history is the objects one edit changed, with what each was before and is
//! after, so undoing is putting the old ones back and redoing the new ones; steps share the objects with the overlay instead of
//! copying it.
//!
//! The history is bounded ([`MAX_UNDO_STEPS`] steps, and with the objects in front of the file together within the memory the
//! plan sets aside for edits): the oldest step that does not fit any more becomes part of the document for good.

use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use super::wipe;
use crate::document::Document;
use crate::edit::signature::{self, Policy};
use crate::edit::{self, Change};
use crate::object::ObjRef;
use crate::xref::XrefEntry;

/// Most edits that can be undone.
pub const MAX_UNDO_STEPS: usize = 100;

/// How an edit, an undo or a redo came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EditStatus {
    /// Done: the document has changed (the page `page` is to be drawn again).
    Done = 0,
    /// The file's author did not allow adding annotations and the file was not opened with the owner password.
    Denied = 1,
    /// Not done (`message` says why); the file is as it was.
    Failed = 2,
    /// Nothing to undo or redo.
    Nothing = 3,
    /// A print job is running; edit when it is over.
    Busy = 4,
}

/// The answer to an edit, an undo or a redo.
#[derive(Clone, Debug, PartialEq)]
pub struct EditResult {
    pub doc: u64,
    pub id: u64,
    pub status: EditStatus,
    /// In English, for a status line. For [`EditStatus::Failed`] the reason. For [`EditStatus::Done`] normally empty: it holds
    /// a warning, told once for a file, when the file is signed and the signature will show it as changed after saving.
    pub message: String,
    /// The page that changed (from 0).
    pub page: u32,
    pub can_undo: bool,
    pub can_redo: bool,
    /// The file differs from what was last saved (or opened).
    pub dirty: bool,
    /// The file's cross-reference data was damaged (or it had something before its header), so it was written afresh as a
    /// whole and will be saved as a whole: the bytes of the original do not stay as they are.
    pub rewrote: bool,
}

/// How a save came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SaveStatus {
    Done = 0,
    /// Not saved (`message` says why); the file that was there is as it was.
    Failed = 1,
}

/// One edit that can be undone.
struct Step {
    page: u32,
    /// The objects it changed, with what they were before and are after.
    changes: Vec<Change>,
    /// About how many bytes the objects of `changes` take.
    bytes: usize,
    /// The state of the document before and after it.
    before: u64,
    after: u64,
}

fn weigh(changes: &[Change]) -> usize {
    changes.iter().fold(0usize, |sum, c| {
        let one = |o: &Option<Rc<crate::object::Object>>| o.as_ref().map_or(0, |o| o.approx_size().saturating_add(48));
        sum.saturating_add(one(&c.before)).saturating_add(one(&c.after))
    })
}

pub(crate) struct Session {
    /// The password the file was opened with, to open it again after a rewrite.
    password: String,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// A number for the state of the document now, the one it was in when last saved, and the next one to give out. Opening
    /// gives 0.
    state: u64,
    saved: u64,
    next: u64,
    rewrote: bool,
    /// What the objects in front of the file and the history may take together, in bytes (from the memory plan).
    budget: usize,
    /// What the file's signatures allow, looked at when the first edit comes; and whether the person has been told that the
    /// signature will show the file as changed.
    policy: Option<Policy>,
    warned: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        wipe(&mut self.password);
    }
}

impl Session {
    pub(crate) fn new(password: &str, budget: u64) -> Session {
        Session {
            password: password.to_string(),
            undo: Vec::new(),
            redo: Vec::new(),
            state: 0,
            saved: 0,
            next: 1,
            rewrote: false,
            budget: usize::try_from(budget).unwrap_or(usize::MAX),
            policy: None,
            warned: false,
        }
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn dirty(&self) -> bool {
        self.state != self.saved
    }

    /// The document has been written as it is now.
    pub(crate) fn mark_saved(&mut self) {
        self.saved = self.state;
    }

    /// What the file's signatures allow an edit to do (looked at once, when the first edit comes).
    pub(crate) fn policy(&mut self, doc: &Document) -> Policy {
        self.policy.get_or_insert_with(|| signature::policy(doc)).clone()
    }

    /// Is the person to be told, now, that the signature will show the file as changed? Yes once.
    pub(crate) fn take_warning(&mut self) -> bool {
        !std::mem::replace(&mut self.warned, true)
    }

    fn history_bytes(&self) -> usize {
        self.undo.iter().chain(&self.redo).fold(0usize, |sum, s| sum.saturating_add(s.bytes))
    }

    /// Make room for `extra` more bytes in front of the file: the oldest steps of the history that do not fit go (what they did
    /// stays done). `false`: the objects in front of the file alone would be over what is allowed; save the file.
    pub(crate) fn make_room(&mut self, doc: &Document, extra: usize) -> bool {
        while doc.overlay_bytes().saturating_add(self.history_bytes()).saturating_add(extra) > self.budget {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
        }
        doc.overlay_bytes().saturating_add(extra) <= self.budget
    }

    /// An edit has changed the document (`changes`, from `edit::commit`): it is the new state, and what was to be redone is gone.
    pub(crate) fn push(&mut self, page: u32, changes: Vec<Change>) -> u32 {
        let (before, after) = (self.state, self.next);
        self.next += 1;
        self.state = after;
        self.redo.clear();
        let bytes = weigh(&changes);
        self.undo.push(Step { page, changes, bytes, before, after });
        if self.undo.len() > MAX_UNDO_STEPS {
            self.undo.remove(0);
        }
        page
    }

    /// Take the last edit back. Returns the page it concerns.
    pub(crate) fn undo(&mut self, doc: &Document) -> Option<u32> {
        let step = self.undo.pop()?;
        edit::replay(doc, &step.changes, false);
        self.state = step.before;
        let page = step.page;
        self.redo.push(step);
        Some(page)
    }

    /// Do again what was taken back. Returns the page it concerns.
    pub(crate) fn redo(&mut self, doc: &Document) -> Option<u32> {
        let step = self.redo.pop()?;
        edit::replay(doc, &step.changes, true);
        self.state = step.after;
        let page = step.page;
        self.undo.push(step);
        Some(page)
    }

    /// The document was written afresh from nothing it can be taken back to: no history before it.
    pub(crate) fn rewritten(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.rewrote = true;
        self.state = self.next;
        self.next += 1;
    }

    /// The objects in front of the file are about to become part of it (a save, `Document::append_section`): what a step took
    /// back to "the file's own object" must from now on take back to the file as it was, which the document still has.
    /// Call this before the document is changed.
    pub(crate) fn before_saving(&mut self, doc: &Document) {
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            for c in &mut step.changes {
                let in_file = matches!(doc.xref_entry(c.num), Some(XrefEntry::InUse { .. } | XrefEntry::Compressed { .. }));
                if c.before.is_none()
                    && in_file
                    && doc.overlay_has(c.num)
                    && let Ok(old) = doc.get_from_file(ObjRef::new(c.num, 0))
                {
                    let old = Rc::new(old);
                    step.bytes = step.bytes.saturating_add(old.approx_size().saturating_add(48));
                    c.before = Some(old);
                }
            }
        }
    }

    pub(crate) fn result(&self, doc: u64, id: u64, status: EditStatus, message: &str, page: u32) -> EditResult {
        EditResult {
            doc,
            id,
            status,
            message: message.to_string(),
            page,
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            dirty: self.dirty(),
            rewrote: self.rewrote,
        }
    }
}

/// Write `bytes` to `path` without ever leaving a half-written file there: into a file of its own in the same folder, which
/// is flushed to the disk and then renamed over `path`. If anything goes wrong the file that was there is as it was.
pub(crate) fn write_atomic(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let target = Path::new(path);
    let name = target.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".qingpdf-{}.tmp", std::process::id()));
    let temp = dir.join(temp_name);
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Object;

    fn doc() -> Document {
        Document::from_bytes(crate::testutil::sample_pdf()).unwrap()
    }

    /// A change of object `num` to the integer `value`, put in front of the document and returned for the history.
    fn change(d: &Document, num: u32, value: i64) -> Vec<Change> {
        edit::commit(d, vec![(num, Object::Integer(value))])
    }

    fn value(d: &Document, num: u32) -> Option<i64> {
        d.get(ObjRef::new(num, 0)).ok().and_then(|o| o.as_int())
    }

    #[test]
    fn undo_and_redo_walk_the_states_and_dirty_follows() {
        let d = doc();
        let mut s = Session::new("", 1 << 20);
        assert!(!s.dirty() && !s.can_undo() && !s.can_redo());
        let c = change(&d, 50, 1);
        s.push(0, c);
        assert!(s.dirty() && s.can_undo() && !s.can_redo());
        s.mark_saved();
        assert!(!s.dirty());
        let c = change(&d, 50, 2);
        s.push(0, c);
        assert!(s.dirty());
        assert_eq!(value(&d, 50), Some(2));
        s.undo(&d);
        assert!(!s.dirty() && s.can_redo());
        assert_eq!(value(&d, 50), Some(1));
        s.undo(&d);
        assert!(s.dirty() && !s.can_undo());
        assert_eq!(value(&d, 50), None, "the object that was new is gone");
        s.redo(&d);
        assert!(!s.dirty());
        assert_eq!(value(&d, 50), Some(1));
        // A new edit forgets what could be redone, and a different edit is a different state even if it changes the same thing.
        s.undo(&d);
        let c = change(&d, 50, 3);
        s.push(0, c);
        assert!(!s.can_redo() && s.dirty());
    }

    #[test]
    fn the_history_is_bounded_and_the_saved_state_can_fall_out_of_it() {
        let d = doc();
        let mut s = Session::new("", 1 << 30);
        s.mark_saved();
        for i in 0..MAX_UNDO_STEPS + 20 {
            let c = change(&d, 50, i as i64);
            s.push(0, c);
        }
        assert_eq!(s.undo.len(), MAX_UNDO_STEPS);
        // Undo all the way: the state before the oldest kept step is not the saved one any more.
        while s.undo(&d).is_some() {}
        assert!(s.dirty());
        assert_eq!(value(&d, 50), Some(19), "what the dropped steps did stays done");
    }

    #[test]
    fn what_is_in_front_of_the_file_and_the_history_stay_within_the_budget() {
        let d = doc();
        let one = change(&d, 60, 1);
        let each = weigh(&one) + 200;
        let mut s = Session::new("", u64::try_from(each * 4).unwrap());
        s.push(0, one);
        for i in 0..10 {
            if !s.make_room(&d, 100) {
                break;
            }
            let c = change(&d, 61 + i, 7);
            s.push(0, c);
        }
        assert!(d.overlay_bytes() + s.history_bytes() <= each * 4 + 100, "{} + {}", d.overlay_bytes(), s.history_bytes());
        // More than the budget in front of the file alone is refused.
        assert!(!s.make_room(&d, each * 40));
    }

    fn annots_count(d: &Document) -> usize {
        let pages = d.pages().unwrap();
        let page = d.get(pages[0].obj_ref).unwrap();
        d.resolve(page.as_dict().unwrap().get("Annots").unwrap_or(&crate::object::Object::Null)).unwrap().as_array().map_or(0, <[crate::object::Object]>::len)
    }

    #[test]
    fn undoing_past_a_save_goes_back_to_the_file_as_it_was() {
        let mut d = doc();
        let mut s = Session::new("", 1 << 20);
        let pages = d.pages().unwrap();
        let edit = edit::Edit::Markup { page: 0, kind: edit::MarkupKind::Highlight, rects: vec![[10.0, 10.0, 80.0, 24.0]], color: [255, 255, 0], author: String::new() };
        let made = edit::commit(&d, edit::apply(&d, &pages, &edit, "D:20260101000000Z").unwrap());
        s.push(0, made);
        assert_eq!(annots_count(&d), 1);
        // Saved: what was in front of the file is in it, and the document is the new file.
        s.before_saving(&d);
        let section = edit::save_section(&d).unwrap();
        d.append_section(&section, 1).unwrap();
        s.mark_saved();
        assert!(!d.has_overlay() && !s.dirty());
        assert_eq!(annots_count(&d), 1);
        // The edit can still be taken back (the page of the file as it was before the save comes back) and done again.
        assert_eq!(s.undo(&d), Some(0));
        assert_eq!(annots_count(&d), 0);
        assert!(s.dirty());
        assert_eq!(s.redo(&d), Some(0));
        assert_eq!(annots_count(&d), 1);
        assert!(!s.dirty());
    }

    #[test]
    fn a_file_is_written_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("qingpdf-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.pdf");
        let path_text = path.to_string_lossy().into_owned();
        std::fs::write(&path, b"old").unwrap();
        write_atomic(&path_text, b"new contents").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
        // A place that cannot be written leaves the old file alone and no temporary file behind.
        let bad = dir.join("no-such-folder").join("b.pdf");
        assert!(write_atomic(&bad.to_string_lossy(), b"x").is_err());
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
