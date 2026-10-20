//! Writing the bookmarks (2026-10-20): the list of bookmarks as the person has it (`bookmarks.rs`) turned into the objects of `/Outlines` (12.3.3).
//!
//! The whole list is what an edit says, and the objects that change are the ones it writes: an item is its old dictionary with the
//! keys that tie the tree together (`/Parent`, `/Prev`, `/Next`, `/First`, `/Last`, `/Count`) and, when the title or the place was
//! changed, `/Title` and `/Dest` put right. Everything else in it (`/C`, `/F`, `/SE`, an action that is not a go-to) stays as the file has
//! it, and an item whose new dictionary is the old one is not written at all. Items that are no longer in the list are left in the file
//! where nothing leads to them; named destinations are never touched, an item keeps the name it has until its place is set.

use super::content::Added;
use super::signature::{self, Policy};
use super::{Ctx, EditClass, LIMIT, commit, replay, save_section, text_string};
use crate::bookmarks;
use crate::idmap::{IdMap, IdSet};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::info::{decode_text_string, pdf_doc_char};
use crate::object::{Dict, ObjRef, Object, PdfString};
use crate::render::work::Work;
use crate::text::Shown;
use crate::view::{MAX_TITLE_CHARS, OutlineItem};

fn odd(what: &str) -> Error {
    Error::Invalid(what.to_string())
}

/// A title as a text string (7.9.2.2): PDFDocEncoding when every character is in it, otherwise UTF-16BE behind its byte order mark.
/// Control characters (and so line ends) are spaces.
fn title_string(title: &str) -> Object {
    let clean: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    // The byte PDFDocEncoding has for a character (the bytes in the gaps of the table read as U+FFFD, which is no character to write).
    let doc_byte = |c: char| if c == '\u{FFFD}' { None } else { (0x18u8..=0x1F).chain(0x80..=0xFF).find(|&b| pdf_doc_char(b) == c) };
    let encoded: Option<Vec<u8>> = clean.chars().map(|c| if c.is_ascii() { Some(c as u8) } else { doc_byte(c) }).collect();
    match encoded {
        Some(bytes) => Object::String(PdfString::literal(bytes)),
        None => text_string(&clean),
    }
}

/// The destination `[page /XYZ left top null]` (12.3.2.2) of the place `x`, `y` (points of the page as shown from its top left corner) of
/// the page of the item; the top left corner of the page where the item has none.
fn destination(pages: &[Page], item: &OutlineItem) -> Result<Object> {
    let page = item.page.and_then(|p| pages.get(p as usize)).ok_or_else(|| odd("a bookmark leads to a page that is not in the file"))?;
    let place = |v: Option<f32>| {
        let v = f64::from(v.unwrap_or(0.0));
        if v.is_finite() && v.abs() <= LIMIT { Ok(v) } else { Err(odd("a bookmark leads to a place that is not on the page")) }
    };
    let (left, top) = Shown::of(page).unmap(place(item.x)?, place(item.y)?);
    Ok(Object::Array(vec![Object::Ref(page.obj_ref), Object::from("XYZ"), Object::Real(left), Object::Real(top), Object::Null]))
}

/// The title the old item has, as the reader reads it.
fn old_title(doc: &Document, old: &Dict) -> Option<String> {
    match old.get("Title").and_then(|t| doc.resolve(t).ok())? {
        Object::String(s) => Some(decode_text_string(s.bytes.get(..s.bytes.len().min(4 * MAX_TITLE_CHARS)).unwrap_or(&[])).chars().take(MAX_TITLE_CHARS).collect()),
        _ => None,
    }
}

/// Put `value` at `key`, or take the key away when there is none.
fn put(d: &mut Dict, key: &str, value: Option<ObjRef>) {
    match value {
        Some(r) => d.set(key, Object::Ref(r)),
        None => d.set(key, Object::Null),
    }
}

/// Work out the objects that make the file's bookmarks the `items`.
pub(super) fn apply(ctx: &mut Ctx<'_>, pages: &[Page], items: &[OutlineItem]) -> Result<()> {
    bookmarks::check(items).map_err(|m| odd(&m))?;
    let doc = ctx.doc;
    // What the file has: the items by object number.
    let mut have: IdMap<u32, (ObjRef, Dict)> = IdMap::new();
    let cut = crate::view::walk_outline(doc, ctx.work, |r, d, _| {
        have.insert(r.num, (r, d.clone()));
    });
    // Bookmarks that were not all read (too many, too deep, too much work) cannot be edited, for the ones not read would be lost; a list that is
    // all new (`bookmarks --import`, `--auto`) replaces them whole, and needs none of them.
    if cut && items.iter().any(|i| i.id != 0) {
        return Err(Error::Limit("the bookmarks of this file are more than can be edited here (too many, too deep, or too much work)".to_string()));
    }
    let mut named: IdSet<u32> = IdSet::new();
    for item in items.iter().filter(|i| i.id != 0) {
        if !have.contains_key(&item.id) || !named.insert(item.id) {
            return Err(odd("the bookmarks of the file are not the ones that were changed: reload the file"));
        }
    }
    let from = ctx.objects.len();
    let mut catalog = doc.catalog()?;
    let catalog_ref = match doc.trailer().get("Root") {
        Some(Object::Ref(r)) => *r,
        _ => return Err(odd("the file has no catalog")),
    };
    // The outline itself: the object the catalog points to, or a new one.
    let existing = match catalog.get("Outlines") {
        Some(Object::Ref(r)) => match doc.get(*r) {
            Ok(Object::Dict(d)) => Some((*r, d)),
            _ => None,
        },
        _ => None,
    };
    let (root_ref, mut root, old_root) = match existing {
        Some((r, d)) => (r, d.clone(), Some(d)),
        None => {
            let base = catalog.get("Outlines").and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
            (ctx.alloc()?, base, None)
        }
    };
    if old_root.is_none() {
        if items.is_empty() {
            return Err(odd("the file has no bookmarks"));
        }
        catalog.set("Outlines", Object::Ref(root_ref));
        ctx.objects.push((catalog_ref.num, Object::Dict(catalog)));
    }

    // The objects of the items, and how they stand to each other.
    let n = items.len();
    let mut refs: Vec<ObjRef> = Vec::with_capacity(n);
    for item in items {
        refs.push(match have.get(&item.id) {
            Some((r, _)) => *r,
            None => ctx.alloc()?,
        });
    }
    let (mut parent, mut prev, mut next, mut first, mut last) = (vec![None; n], vec![None; n], vec![None; n], vec![None::<usize>; n], vec![None::<usize>; n]);
    let (mut top_first, mut top_last): (Option<usize>, Option<usize>) = (None, None);
    let mut chain: Vec<usize> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        chain.truncate(item.depth as usize);
        let up = chain.last().copied();
        if let Some(p) = parent.get_mut(i) {
            *p = up;
        }
        let before = match up {
            Some(u) => last.get(u).copied().flatten(),
            None => top_last,
        };
        if let Some(b) = before.and_then(|b| next.get_mut(b)) {
            *b = Some(i);
        }
        if let Some(p) = prev.get_mut(i) {
            *p = before;
        }
        match up {
            Some(u) => {
                if first.get(u).copied().flatten().is_none()
                    && let Some(f) = first.get_mut(u)
                {
                    *f = Some(i);
                }
                if let Some(l) = last.get_mut(u) {
                    *l = Some(i);
                }
            }
            None => {
                top_first = top_first.or(Some(i));
                top_last = Some(i);
            }
        }
        chain.push(i);
    }
    let (counts, top_count) = bookmarks::counts(items);
    let at = |list: &[Option<usize>], i: usize| list.get(i).copied().flatten().and_then(|j| refs.get(j).copied());

    for (i, item) in items.iter().enumerate() {
        let old = have.get(&item.id).map(|(_, d)| d).filter(|_| item.id != 0);
        let mut d = old.cloned().unwrap_or_default();
        if old.is_none_or(|o| old_title(doc, o).as_deref() != Some(item.title.as_str())) {
            d.set("Title", title_string(&item.title));
        }
        d.set("Parent", Object::Ref(at(&parent, i).unwrap_or(root_ref)));
        put(&mut d, "Prev", at(&prev, i));
        put(&mut d, "Next", at(&next, i));
        put(&mut d, "First", at(&first, i));
        put(&mut d, "Last", at(&last, i));
        match counts.get(i).copied() {
            Some(c) if c != 0 => d.set("Count", Object::Integer(c)),
            _ => d.set("Count", Object::Null),
        }
        if item.placed && !item.action && item.page.is_some() {
            d.set("Dest", destination(pages, item)?);
            d.set("A", Object::Null);
        }
        if old != Some(&d) {
            ctx.objects.push((refs.get(i).map_or(0, |r| r.num), Object::Dict(d)));
        }
    }

    root.set("Type", Object::from("Outlines"));
    put(&mut root, "First", top_first.and_then(|i| refs.get(i).copied()));
    put(&mut root, "Last", top_last.and_then(|i| refs.get(i).copied()));
    root.set("Count", if top_count != 0 { Object::Integer(top_count) } else { Object::Null });
    if old_root.as_ref() != Some(&root) {
        ctx.objects.push((root_ref.num, Object::Dict(root)));
    }
    ctx.tally(from)
}

/// What the command line may spend on the objects of an edit.
const CLI_ROOM: usize = 512 << 20;

/// Why the bookmarks of a file may not be changed, if they may not: its permissions (bit 11, "assemble", or bit 4, unless it was opened
/// with the owner password) and its signatures (the same as for the pages).
pub fn check_outline_allowed(doc: &Document, password: &str) -> Result<()> {
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    if let Some(encryption) = doc.encryption()
        && !encryption.permissions.assemble
        && !encryption.permissions.modify
        && !crate::ops::has_owner_rights(doc, &encryption, password)
    {
        return Err(odd("the file's author did not allow changing its bookmarks (permission bit 11 or 4); the owner password is needed (give it with --password)"));
    }
    match signature::look(doc).policy(EditClass::Assemble) {
        Policy::Refuse(why) => Err(Error::Invalid(why.to_string())),
        _ => Ok(()),
    }
}

/// The file with its bookmarks made `items` (one incremental update behind the whole original; a file with damaged structure data is
/// written afresh first).
pub fn set_outline(doc: &Document, password: &str, items: Vec<OutlineItem>) -> Result<Added> {
    check_outline_allowed(doc, password)?;
    let rewritten = doc.was_repaired() || doc.leading_junk() > 0;
    let fresh = if rewritten { Some(Document::from_bytes_with_password(crate::ops::copy_all(doc)?.data, password)?) } else { None };
    let doc = fresh.as_ref().unwrap_or(doc);
    let pages = doc.pages()?;
    // (Not through `apply_any`: that would bring the code of the page changes into the command line.)
    let work = Work::new();
    let mut ctx = Ctx { doc, next: doc.next_free_number(), objects: Vec::new(), work: &work, room: CLI_ROOM, used: 0, notes: Vec::new() };
    apply(&mut ctx, &pages, &items)?;
    let (objects, warnings) = (ctx.objects, ctx.notes);
    // The change is put in front of the document's objects to be written, and taken off again: the document is as it was.
    let changes = commit(doc, objects);
    let section = save_section(doc);
    replay(doc, &changes, false);
    let section = section?;
    let mut data = Vec::with_capacity(doc.data().len() + section.len());
    data.extend_from_slice(doc.data());
    data.extend_from_slice(&section);
    Ok(Added { data, rewritten, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_pdfdocencoding_when_it_can_be_and_utf16_when_it_cannot() {
        let bytes = |o: Object| match o {
            Object::String(s) => (s.bytes, s.hex),
            _ => panic!("not a string"),
        };
        assert_eq!(bytes(title_string("Chapter 1")), (b"Chapter 1".to_vec(), false));
        // Latin-1 letters and the bullet are in PDFDocEncoding.
        assert_eq!(bytes(title_string("Caf\u{e9} \u{2022}")), (vec![b'C', b'a', b'f', 0xE9, b' ', 0x80], false));
        // Chinese is not.
        assert_eq!(bytes(title_string("\u{4E2D}\u{6587}")), (vec![0xFE, 0xFF, 0x4E, 0x2D, 0x65, 0x87], true));
        // A line end is a space.
        assert_eq!(bytes(title_string("a\nb")), (b"a b".to_vec(), false));
        // A title that PDFDocEncoding reads back as it was.
        for title in ["Caf\u{e9}", "\u{201c}quoted\u{201d}", "\u{20ac}5", "\u{4E2D}\u{6587} and English"] {
            let Object::String(s) = title_string(title) else { panic!("not a string") };
            assert_eq!(decode_text_string(&s.bytes), title);
        }
    }
}
