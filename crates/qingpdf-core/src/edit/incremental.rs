//! An incremental update (ISO 32000-1 7.5.6): objects appended after the end of the file, and a new cross-reference
//! section that points back (`/Prev`) at the old one. Not one byte of the file before it changes.
//!
//! The new section is a classic table when the file's newest section is one (a hybrid file's newest section is a table
//! too, 7.5.8.4: its `/XRefStm` stays in the older section it belongs to), and a cross-reference stream (7.5.8) when the
//! newest section is a stream. `/ID` keeps its first string and gets a new second one (14.4); an encrypted file keeps its
//! encryption: every string and stream written is encrypted under the object's own number and generation (7.6.2), the
//! cross-reference stream and the encryption dictionary are not.

use std::rc::Rc;

use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, MAX_OBJECT_NUMBER, Object, PdfString, Stream};
use crate::security::OutputCrypt;
use crate::writer::{file_id, write_indirect_gen, write_object};
use crate::xref::{self, XrefEntry};

/// The bytes to append to `doc`'s file so that `objects` (by number; a new version of an object that is in the file keeps
/// the generation the file gives it, a new object has generation 0) take effect. `doc` is the file as it stands.
pub(crate) fn section(doc: &Document, objects: &[(u32, Rc<Object>)]) -> Result<Vec<u8>> {
    let base = doc.data();
    let prefix = u64::try_from(base.len()).unwrap_or(u64::MAX);
    let (prev, is_stream) = xref::newest_section(base)?;
    let crypt = match doc.security() {
        Some(security) => Some(OutputCrypt::new(security).ok_or(Error::PasswordRequired)?),
        None => None,
    };

    let mut out: Vec<u8> = Vec::new();
    // The file may end without a line end after its `%%EOF`.
    out.push(b'\n');
    let mut entries: Vec<(u32, u16, u64)> = Vec::with_capacity(objects.len() + 1);
    for (num, object) in objects {
        let num = *num;
        if num == 0 || num > MAX_OBJECT_NUMBER {
            return Err(Error::Invalid(format!("object number {num} is out of range")));
        }
        // The generation is the file's own (7.5.6); the key of an encrypted file depends on it.
        let generation = match doc.xref_entry(num) {
            Some(XrefEntry::InUse { generation, .. }) => generation,
            _ => 0,
        };
        entries.push((num, generation, prefix + u64::try_from(out.len()).unwrap_or(0)));
        let enc = crypt.as_ref().map(|c| c.object_with_generation(num, generation, false));
        write_indirect_gen(&mut out, num, generation, object, enc.as_ref())?;
    }
    entries.sort_unstable_by_key(|e| e.0);
    if entries.windows(2).any(|w| matches!(w, [a, b] if a.0 == b.0)) {
        return Err(Error::Invalid("an object is written twice in one update".to_string()));
    }

    let highest = entries.last().map_or(0, |e| e.0);
    let mut size = doc.next_free_number().max(highest.saturating_add(1));
    let id1 = {
        // The new identifier covers the file and what is being added to it (14.4).
        let mut hasher_input = Vec::with_capacity(out.len() + 64);
        hasher_input.extend_from_slice(base.get(base.len().saturating_sub(4096)..).unwrap_or(&[]));
        hasher_input.extend_from_slice(&out);
        file_id(&hasher_input)
    };

    // The entries of the trailer that every section carries.
    let mut trailer = Dict::new();
    if let Some(root) = doc.trailer().get("Root") {
        trailer.set("Root", root.clone());
    } else {
        return Err(Error::Invalid("the trailer has no /Root".to_string()));
    }
    for key in ["Info", "Encrypt"] {
        if let Some(v) = doc.trailer().get(key) {
            trailer.set(key, v.clone());
        }
    }
    // An encrypted file's key (revisions 2 to 4) is made from the first string of `/ID`: it stays as it is, empty or not, and a
    // file that has no `/ID` is not given one (the key would change and nothing could open it).
    match (first_id(doc), doc.is_encrypted()) {
        (Some(id0), true) => set_id(&mut trailer, id0, id1.to_vec()),
        (None, true) => {}
        (Some(id0), false) if !id0.is_empty() => set_id(&mut trailer, id0, id1.to_vec()),
        (_, false) => set_id(&mut trailer, id1.to_vec(), id1.to_vec()),
    }
    trailer.set("Prev", Object::Integer(i64::try_from(prev).unwrap_or(i64::MAX)));

    if is_stream {
        // The stream is an object of the file too, the next number after the others.
        let at = prefix + u64::try_from(out.len()).unwrap_or(0);
        let num = size;
        if num > MAX_OBJECT_NUMBER {
            return Err(Error::Limit("too many objects".to_string()));
        }
        size = num + 1;
        entries.push((num, 0, at));
        let stream = xref_stream(&entries, size, trailer)?;
        write_indirect_gen(&mut out, num, 0, &Object::Stream(stream), None)?;
        out.extend_from_slice(format!("startxref\n{at}\n%%EOF\n").as_bytes());
    } else {
        let at = prefix + u64::try_from(out.len()).unwrap_or(0);
        out.extend_from_slice(b"xref\n");
        let mut i = 0;
        while i < entries.len() {
            // One subsection for each run of consecutive numbers (7.5.4).
            let mut j = i + 1;
            while let (Some(a), Some(b)) = (entries.get(j - 1), entries.get(j)) {
                if b.0 != a.0 + 1 {
                    break;
                }
                j += 1;
            }
            let run = entries.get(i..j).unwrap_or(&[]);
            out.extend_from_slice(format!("{} {}\n", run.first().map_or(0, |e| e.0), run.len()).as_bytes());
            for &(_, generation, offset) in run {
                if offset > 9_999_999_999 {
                    return Err(Error::Limit("the file is too large for a classic cross-reference table".to_string()));
                }
                out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
            }
            i = j;
        }
        trailer.set("Size", Object::Integer(i64::from(size)));
        out.extend_from_slice(b"trailer\n");
        write_object(&mut out, &Object::Dict(trailer))?;
        out.extend_from_slice(format!("\nstartxref\n{at}\n%%EOF\n").as_bytes());
    }
    Ok(out)
}

fn set_id(trailer: &mut Dict, first: Vec<u8>, second: Vec<u8>) {
    trailer.set("ID", Object::Array(vec![Object::String(PdfString::hex(first)), Object::String(PdfString::hex(second))]));
}

/// The cross-reference stream (7.5.8) for `entries` (sorted by number): widths 1, 4 (8 for a file past 4 GiB) and 2.
fn xref_stream(entries: &[(u32, u16, u64)], size: u32, mut trailer: Dict) -> Result<Stream> {
    let widest = entries.iter().map(|e| e.2).max().unwrap_or(0);
    let offset_bytes: usize = if widest > u64::from(u32::MAX) { 8 } else { 4 };
    let mut rows: Vec<u8> = Vec::with_capacity(entries.len() * (3 + offset_bytes));
    let mut index: Vec<Object> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let mut j = i + 1;
        while let (Some(a), Some(b)) = (entries.get(j - 1), entries.get(j)) {
            if b.0 != a.0 + 1 {
                break;
            }
            j += 1;
        }
        let run = entries.get(i..j).unwrap_or(&[]);
        index.push(Object::Integer(i64::from(run.first().map_or(0, |e| e.0))));
        index.push(Object::Integer(i64::try_from(run.len()).unwrap_or(0)));
        for &(_, generation, offset) in run {
            rows.push(1);
            rows.extend_from_slice(offset.to_be_bytes().get(8 - offset_bytes..).unwrap_or(&[]));
            rows.extend_from_slice(&generation.to_be_bytes());
        }
        i = j;
    }
    trailer.set("Type", Object::from("XRef"));
    trailer.set("Size", Object::Integer(i64::from(size)));
    trailer.set("W", Object::Array(vec![Object::Integer(1), Object::Integer(i64::try_from(offset_bytes).unwrap_or(4)), Object::Integer(2)]));
    trailer.set("Index", Object::Array(index));
    trailer.set("Filter", Object::from("FlateDecode"));
    Ok(Stream { dict: trailer, data: compress_to_vec_zlib(&rows, 6) })
}

/// The first string of the trailer's `/ID` (empty or not), if it has one.
fn first_id(doc: &Document) -> Option<Vec<u8>> {
    let id = doc.resolve(doc.trailer().get("ID")?).ok()?;
    match id.as_array()?.first()? {
        Object::String(s) => Some(s.bytes.clone()),
        _ => None,
    }
}
