//! Writing a brand-new PDF file: object syntax (ISO 32000-1 7.3), file
//! structure (7.5.2 header, 7.5.4 cross-reference table, 7.5.5 trailer).
//!
//! A [`Builder`] collects indirect objects one at a time and serialises each
//! as soon as it is complete, so memory use stays near the size of the output.
//! Objects can be made from scratch or imported from existing documents with
//! renumbering ([`Builder::import`]); only what is reachable from what the
//! caller imports ends up in the file.
//!
//! The output uses a classic cross-reference table and no object streams.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hasher;

use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::deflate::core::{CompressorOxide, TDEFLFlush, TDEFLStatus, compress, create_comp_flags_from_zip_params};

use crate::dests::NamedDests;
use crate::document::Document;
use crate::error::{Error, Result};
use crate::lexer::is_delimiter;
use crate::object::{Dict, Name, ObjRef, Object, PdfString, Stream};

/// A thing the user should know about the result (something was dropped, say).
/// Never an error: the output is valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning(pub String);

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Deepest nesting of arrays and dictionaries the writer will emit. The reader
/// stops at 256, so anything it produced is well inside this.
const MAX_WRITE_DEPTH: usize = 512;

/// Largest byte offset a cross-reference entry can hold (10 digits, 7.5.4).
const MAX_OFFSET: u64 = 9_999_999_999;

/// How many "could not read this object" messages are listed one by one.
const MAX_UNREADABLE_LISTED: usize = 5;

/// Flate compression level for new streams: a good size for little time.
const FLATE_LEVEL: u8 = 6;

/// Flate compression (zlib, RFC 1950) of data that arrives in pieces: the
/// compressed bytes are kept, the input is not, so a stream can be made of
/// data too big to hold in memory all at once (the rows of a huge image).
pub(crate) struct FlateWriter {
    compressor: Box<CompressorOxide>,
    out: Vec<u8>,
    scratch: Vec<u8>,
}

impl FlateWriter {
    pub(crate) fn new() -> Self {
        let flags = create_comp_flags_from_zip_params(i32::from(FLATE_LEVEL), 1, 0);
        FlateWriter { compressor: Box::new(CompressorOxide::new(flags)), out: Vec::new(), scratch: vec![0; 1 << 16] }
    }

    /// Compress `data` (more of the stream's input).
    pub(crate) fn write(&mut self, mut data: &[u8]) -> Result<()> {
        loop {
            let (status, consumed, produced) = compress(&mut self.compressor, data, &mut self.scratch, TDEFLFlush::None);
            self.out.extend_from_slice(self.scratch.get(..produced).unwrap_or(&[]));
            data = data.get(consumed..).unwrap_or(&[]);
            if !matches!(status, TDEFLStatus::Okay) {
                return Err(Error::Invalid("internal error: Flate compression failed".to_string()));
            }
            if data.is_empty() && produced < self.scratch.len() {
                return Ok(());
            }
        }
    }

    /// End the stream and give the compressed bytes.
    pub(crate) fn finish(mut self) -> Result<Vec<u8>> {
        loop {
            let (status, _, produced) = compress(&mut self.compressor, &[], &mut self.scratch, TDEFLFlush::Finish);
            self.out.extend_from_slice(self.scratch.get(..produced).unwrap_or(&[]));
            match status {
                TDEFLStatus::Done => return Ok(self.out),
                TDEFLStatus::Okay => {}
                _ => return Err(Error::Invalid("internal error: Flate compression failed".to_string())),
            }
        }
    }
}

// --- object syntax ------------------------------------------------------------

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// A non-negative integer in decimal, without going through `format!`: a
/// large file has millions of these.
fn push_number(out: &mut Vec<u8>, mut n: u64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    loop {
        at -= 1;
        if let Some(slot) = digits.get_mut(at) {
            *slot = b'0' + u8::try_from(n % 10).unwrap_or(0);
        }
        n /= 10;
        if n == 0 || at == 0 {
            break;
        }
    }
    out.extend_from_slice(digits.get(at..).unwrap_or(&[]));
}

fn push_hex(out: &mut Vec<u8>, b: u8) {
    out.push(*HEX_UPPER.get(usize::from(b >> 4)).unwrap_or(&b'0'));
    out.push(*HEX_UPPER.get(usize::from(b & 15)).unwrap_or(&b'0'));
}

/// A name (7.3.5): `/` and then each byte as itself if it is a regular
/// character from `!` to `~` other than `#`, and as `#xx` otherwise (white
/// space, delimiters, `#`, anything outside 21h-7Eh).
pub fn write_name(out: &mut Vec<u8>, name: &Name) {
    out.push(b'/');
    for &b in name.as_bytes() {
        if (0x21..=0x7E).contains(&b) && b != b'#' && !is_delimiter(b) {
            out.push(b);
        } else {
            out.push(b'#');
            push_hex(out, b);
        }
    }
}

/// A literal string (7.3.4.2): `(`, `)` and `\` escaped with a backslash;
/// every byte outside printable ASCII as a three-digit octal escape (always
/// three digits, so a following digit cannot be mistaken for part of it).
/// Carriage returns and line feeds are escaped too, because an end-of-line
/// inside a literal string is read back as LF whatever it was.
pub fn write_literal_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            0x20..=0x7E => out.push(b),
            _ => {
                out.push(b'\\');
                out.push(b'0' + (b >> 6));
                out.push(b'0' + ((b >> 3) & 7));
                out.push(b'0' + (b & 7));
            }
        }
    }
    out.push(b')');
}

/// A hexadecimal string (7.3.4.3).
pub fn write_hex_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'<');
    for &b in bytes {
        push_hex(out, b);
    }
    out.push(b'>');
}

/// A real number (7.3.3): Rust's shortest decimal form that reads back as the
/// same value. `Display` for `f64` never uses an exponent, which the spec
/// forbids. A whole number gets `.0` so that it stays a real when read back.
pub fn format_real(v: f64) -> Result<String> {
    if !v.is_finite() {
        return Err(Error::Invalid("cannot write a number that is not finite".to_string()));
    }
    let mut s = format!("{v}");
    if !s.contains('.') {
        s.push_str(".0");
    }
    Ok(s)
}

/// Write `obj` as a direct object. A stream cannot be direct (7.3.8.1: it
/// must be an indirect object), so one here is an error. Dictionary entries
/// whose value is null are left out (7.3.7: same as absent).
pub fn write_object(out: &mut Vec<u8>, obj: &Object) -> Result<()> {
    write_depth(out, obj, 0)
}

fn write_depth(out: &mut Vec<u8>, obj: &Object, depth: usize) -> Result<()> {
    if depth > MAX_WRITE_DEPTH {
        return Err(Error::Limit("objects nested too deeply to write".to_string()));
    }
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(true) => out.extend_from_slice(b"true"),
        Object::Bool(false) => out.extend_from_slice(b"false"),
        Object::Integer(i) => {
            if *i < 0 {
                out.push(b'-');
            }
            push_number(out, i.unsigned_abs());
        }
        Object::Real(r) => out.extend_from_slice(format_real(*r)?.as_bytes()),
        Object::String(PdfString { bytes, hex: true }) => write_hex_string(out, bytes),
        Object::String(PdfString { bytes, hex: false }) => write_literal_string(out, bytes),
        Object::Name(n) => write_name(out, n),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_depth(out, item, depth + 1)?;
            }
            out.push(b']');
        }
        Object::Dict(d) => write_dict(out, d, None, depth)?,
        Object::Stream(_) => {
            return Err(Error::Invalid("a stream cannot be written as a direct object".to_string()));
        }
        Object::Ref(r) => {
            push_number(out, u64::from(r.num));
            out.push(b' ');
            push_number(out, u64::from(r.generation));
            out.extend_from_slice(b" R");
        }
    }
    Ok(())
}

/// `<< /Key value ... >>`, skipping null values and, when `skip` is given, the
/// entry with that key.
fn write_dict(out: &mut Vec<u8>, d: &Dict, skip: Option<&str>, depth: usize) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in d.iter() {
        if matches!(value, Object::Null) || skip.is_some_and(|s| key == s) {
            continue;
        }
        out.push(b' ');
        write_name(out, key);
        out.push(b' ');
        write_depth(out, value, depth + 1)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

/// `n 0 obj ... endobj` (7.3.10). For a stream (7.3.8) the `/Length` entry is
/// replaced by the real length of the data as a direct integer, and the data
/// is written as it is: still encoded, `Filter` and `DecodeParms` untouched.
fn write_indirect(out: &mut Vec<u8>, num: u32, obj: &Object) -> Result<()> {
    push_number(out, u64::from(num));
    out.extend_from_slice(b" 0 obj\n");
    match obj {
        Object::Stream(Stream { dict, data }) => {
            out.extend_from_slice(b"<<");
            for (key, value) in dict.iter() {
                if matches!(value, Object::Null) || key == "Length" {
                    continue;
                }
                out.push(b' ');
                write_name(out, key);
                out.push(b' ');
                write_depth(out, value, 1)?;
            }
            out.extend_from_slice(b" /Length ");
            push_number(out, u64::try_from(data.len()).unwrap_or(u64::MAX));
            out.extend_from_slice(b" >>\nstream\n");
            out.extend_from_slice(data);
            // 7.3.8.1 says there "should" be an end-of-line before
            // `endstream`; it would not count in /Length. None is written: it
            // is a byte per stream for nothing (qpdf writes none either),
            // /Length is exact, and every reader finds `endstream` by it.
            out.extend_from_slice(b"endstream");
        }
        other => write_depth(out, other, 0)?,
    }
    out.extend_from_slice(b"\nendobj\n");
    Ok(())
}

// --- the builder --------------------------------------------------------------

/// For an annotation object number, the pages that list it; and the pages that
/// are kept.
type Exclusion<'a> = (&'a HashMap<u32, Vec<u32>>, &'a HashSet<u32>);

/// One document objects are imported from.
struct Source<'a> {
    doc: &'a Document,
    /// Object numbers of all the pages of the document. A reference to one
    /// that was not registered with [`Builder::register_page`] becomes null.
    pages: &'a HashSet<u32>,
    /// Source object number -> number in the output, or `None` when the
    /// reference becomes null (the target is not part of the output).
    map: HashMap<u32, Option<u32>>,
    /// Turn named destinations into explicit ones through this document's own
    /// name tree (see [`Builder::resolve_named_destinations`]).
    resolve_names: bool,
    names: OnceCell<NamedDests>,
    /// Leave out the keys that point into a structure tree that is not copied.
    drop_structure_links: bool,
    /// Objects whose content is to be taken from here, not from the file.
    replace: HashMap<u32, Object>,
    /// Annotations that no kept page lists are not imported: object number ->
    /// pages that list it, and the pages that are kept.
    exclude: Option<Exclusion<'a>>,
    /// Inherited values written once for all the pages that share them.
    shared: HashMap<usize, ObjRef>,
}

/// An imported object waiting for its references to be translated.
struct Pending {
    source: usize,
    new: ObjRef,
    object: Object,
}

/// Builds one new PDF file. See the module documentation.
pub struct Builder<'a> {
    buf: Vec<u8>,
    /// Where each object starts; index is the object number minus one.
    offsets: Vec<Option<u64>>,
    version: (u8, u8),
    sources: Vec<Source<'a>>,
    pending: Vec<Pending>,
    unreadable: Vec<String>,
    unreadable_total: usize,
}

impl<'a> Builder<'a> {
    /// Start a file. The header is `%PDF-x.y` and then a comment of four
    /// bytes above 127 (7.5.2), so that tools treat the file as binary.
    pub fn new(version: (u8, u8)) -> Self {
        Builder::with_capacity(version, 0)
    }

    /// [`Builder::new`] with room for about `bytes` of output, when a file of
    /// about that size is expected (the next piece of a split, say): the
    /// output is not copied over and over as it grows.
    pub fn with_capacity(version: (u8, u8), bytes: usize) -> Self {
        Builder {
            // A hint, not a promise: never more than 64 MiB up front.
            buf: Vec::with_capacity(bytes.min(64 << 20)),
            offsets: Vec::new(),
            version,
            sources: Vec::new(),
            pending: Vec::new(),
            unreadable: Vec::new(),
            unreadable_total: 0,
        }
    }

    /// The PDF version the header will say.
    pub fn set_version(&mut self, version: (u8, u8)) {
        self.version = version;
    }

    /// Allow importing from `doc`, whose pages have the object numbers in
    /// `pages`. Returns the index to pass to the import functions. The same
    /// document may be added again to get a separate copy of everything
    /// (merging a file with itself, say).
    pub fn add_source(&mut self, doc: &'a Document, pages: &'a HashSet<u32>) -> usize {
        self.sources.push(Source {
            doc,
            pages,
            map: HashMap::new(),
            resolve_names: false,
            names: OnceCell::new(),
            drop_structure_links: false,
            replace: HashMap::new(),
            exclude: None,
            shared: HashMap::new(),
        });
        self.sources.len() - 1
    }

    fn source_mut(&mut self, source: usize) -> Option<&mut Source<'a>> {
        self.sources.get_mut(source)
    }

    /// For a source whose catalog is not copied: a link or go-to action that
    /// names its destination (12.3.2.3) is given the destination itself, found
    /// through the source's own `/Names` `/Dests` tree or `/Dests` dictionary,
    /// so that it cannot end up in somebody else's name tree. A name that
    /// leads nowhere removes the link's destination, or a go-to action.
    pub fn resolve_named_destinations(&mut self, source: usize) {
        if let Some(s) = self.source_mut(source) {
            s.resolve_names = true;
        }
    }

    /// For a source whose structure tree (14.7) is not copied: drop
    /// `/StructParents`, `/StructParent` and `/SE`, which are indexes into it.
    pub fn drop_structure_links(&mut self, source: usize) {
        if let Some(s) = self.source_mut(source) {
            s.drop_structure_links = true;
        }
    }

    /// Take the content of object `num` of `source` from `object` (whose
    /// references are source references) instead of from the file.
    pub fn replace(&mut self, source: usize, num: u32, object: Object) {
        if let Some(s) = self.source_mut(source) {
            s.replace.insert(num, object);
        }
    }

    /// Do not import the annotations of `source` that are listed only by
    /// pages that are not in `kept`: `owners` says, for an annotation object
    /// number, which pages list it. References to them become null.
    pub fn exclude_annotations(&mut self, source: usize, owners: &'a HashMap<u32, Vec<u32>>, kept: &'a HashSet<u32>) {
        if let Some(s) = self.source_mut(source) {
            s.exclude = Some((owners, kept));
        }
    }

    /// Take the next object number without writing anything yet. The object
    /// must be given later with [`Builder::put`].
    pub fn reserve(&mut self) -> Result<ObjRef> {
        let num = u32::try_from(self.offsets.len().saturating_add(1))
            .ok()
            .filter(|&n| n < u32::MAX / 2)
            .ok_or_else(|| Error::Limit("too many objects to write".to_string()))?;
        self.offsets.push(None);
        Ok(ObjRef::new(num, 0))
    }

    /// Write object `r` (obtained from [`Builder::reserve`]). All references
    /// inside `obj` must already be output object numbers.
    pub fn put(&mut self, r: ObjRef, obj: &Object) -> Result<()> {
        if self.buf.is_empty() {
            self.write_header();
        }
        let slot = usize::try_from(r.num)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| self.offsets.get_mut(i))
            .ok_or_else(|| Error::Invalid(format!("object {} was never reserved", r.num)))?;
        if slot.is_some() {
            return Err(Error::Invalid(format!("object {} written twice", r.num)));
        }
        *slot = Some(u64::try_from(self.buf.len()).unwrap_or(u64::MAX));
        write_indirect(&mut self.buf, r.num, obj)
    }

    /// Reserve a number and write `obj` under it.
    pub fn add(&mut self, obj: &Object) -> Result<ObjRef> {
        let r = self.reserve()?;
        self.put(r, obj)?;
        Ok(r)
    }

    /// Add a new stream, compressing `data` with Flate. `dict` must not name
    /// a `/Filter` of its own.
    pub fn add_flate_stream(&mut self, mut dict: Dict, data: &[u8]) -> Result<ObjRef> {
        dict.set("Filter", Object::from("FlateDecode"));
        let packed = compress_to_vec_zlib(data, FLATE_LEVEL);
        self.add(&Object::Stream(Stream { dict, data: packed }))
    }

    /// Add a stream whose `data` is already Flate-compressed.
    pub fn add_precompressed_flate_stream(&mut self, mut dict: Dict, data: Vec<u8>) -> Result<ObjRef> {
        dict.set("Filter", Object::from("FlateDecode"));
        self.add(&Object::Stream(Stream { dict, data }))
    }

    fn write_header(&mut self) {
        self.buf.extend_from_slice(format!("%PDF-{}.{}\n", self.version.0, self.version.1).as_bytes());
        self.buf.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
    }

    // --- importing --------------------------------------------------------------

    /// Say that a reference to the page object `old` in source `source`
    /// becomes the output page `new`. The first registration of a page wins.
    /// Pages never registered become null.
    pub fn register_page(&mut self, source: usize, old: ObjRef, new: ObjRef) {
        if let Some(s) = self.sources.get_mut(source) {
            s.map.entry(old.num).or_insert(Some(new.num));
        }
    }

    /// Copy `obj`, which comes from source `source`, translating every
    /// reference in it to an output object, and import the objects they lead
    /// to (and so on, transitively). The result is ready for [`Builder::put`].
    ///
    /// A reference becomes null when its target is missing, is a page tree
    /// node (`/Type /Pages`, which is how `/Parent` is never followed: page
    /// trees are rebuilt), is a page that was not registered as part of the
    /// output, or is a catalog or a cross-reference/object stream. A stream's
    /// `/Length` is dropped, because the writer sets it.
    pub fn import(&mut self, source: usize, obj: Object) -> Result<Object> {
        let translated = self.translate(source, obj)?;
        self.drain()?;
        Ok(translated)
    }

    /// Import the object `r` of source `source` and what it leads to. `None`
    /// when the reference becomes null.
    pub fn import_ref(&mut self, source: usize, r: ObjRef) -> Result<Option<ObjRef>> {
        let new = self.map_ref(source, r)?;
        self.drain()?;
        Ok(new)
    }

    /// Import `obj` of source `source` as an object of its own, once for every
    /// `id`: what the pages that inherit one direct value share. Returns the
    /// output object.
    pub fn import_shared(&mut self, source: usize, id: usize, obj: &Object) -> Result<ObjRef> {
        if let Some(known) = self.sources.get(source).and_then(|s| s.shared.get(&id)) {
            return Ok(*known);
        }
        let translated = self.import(source, obj.clone())?;
        let new = self.add(&translated)?;
        if let Some(s) = self.source_mut(source) {
            s.shared.insert(id, new);
        }
        Ok(new)
    }

    /// Write the imported objects whose references are still untranslated.
    /// Iterative, so a chain of a hundred thousand outline entries is no
    /// problem for the stack.
    fn drain(&mut self) -> Result<()> {
        while let Some(Pending { source, new, object }) = self.pending.pop() {
            let translated = self.translate(source, object)?;
            self.put(new, &translated)?;
        }
        Ok(())
    }

    /// Translate the references inside `obj`; objects found for the first time
    /// are queued in `pending`.
    fn translate(&mut self, source: usize, obj: Object) -> Result<Object> {
        Ok(match obj {
            Object::Ref(r) => match self.map_ref(source, r)? {
                Some(new) => Object::Ref(new),
                None => Object::Null,
            },
            Object::Array(mut items) => {
                // In place: no new array for every array of a big file.
                for item in &mut items {
                    *item = self.translate(source, std::mem::replace(item, Object::Null))?;
                }
                Object::Array(items)
            }
            Object::Dict(d) => match self.rewrite_dict(source, d) {
                Some(d) => Object::Dict(self.translate_dict(source, d)?),
                None => Object::Null,
            },
            Object::Stream(Stream { mut dict, data }) => {
                dict.remove("Length");
                match self.rewrite_dict(source, dict) {
                    Some(dict) => Object::Stream(Stream { dict: self.translate_dict(source, dict)?, data }),
                    None => Object::Null,
                }
            }
            other => other,
        })
    }

    /// The dictionary as it is to be imported from `source`, or `None` when
    /// it is not to be imported at all (see
    /// [`Builder::resolve_named_destinations`] and
    /// [`Builder::drop_structure_links`]).
    fn rewrite_dict(&self, source: usize, mut d: Dict) -> Option<Dict> {
        let Some(s) = self.sources.get(source) else {
            return Some(d);
        };
        if !s.drop_structure_links && !s.resolve_names {
            return Some(d);
        }
        if s.drop_structure_links {
            for key in ["StructParents", "StructParent", "SE"] {
                d.remove(key);
            }
        }
        if s.resolve_names {
            // A link annotation (12.5.6.5) or an outline item (12.3.3) ...
            if let Some((key, is_name)) = d.get("Dest").and_then(|v| named_key(s.doc, v)) {
                match s.names.get_or_init(|| NamedDests::load(s.doc)).lookup(s.doc, &key, is_name) {
                    Some(array) => d.set("Dest", Object::Array(array)),
                    None => {
                        d.remove("Dest");
                    }
                }
            }
            // ... or a go-to action (12.6.4.2, Table 199).
            if matches!(d.get("S"), Some(Object::Name(n)) if n == "GoTo")
                && let Some((key, is_name)) = d.get("D").and_then(|v| named_key(s.doc, v))
            {
                let array = s.names.get_or_init(|| NamedDests::load(s.doc)).lookup(s.doc, &key, is_name)?;
                d.set("D", Object::Array(array));
            }
        }
        Some(d)
    }

    fn translate_dict(&mut self, source: usize, mut d: Dict) -> Result<Dict> {
        let mut dropped = false;
        for (key, slot) in d.iter_mut() {
            let mut value = self.translate(source, std::mem::replace(slot, Object::Null))?;
            // A kid that became null (a page or field that is not part of the
            // output) is not a kid.
            if key == "Kids"
                && let Object::Array(items) = &mut value
            {
                items.retain(|item| !matches!(item, Object::Null));
            }
            dropped |= matches!(value, Object::Null);
            *slot = value;
        }
        // A reference that became null drops its key (7.3.7).
        if dropped {
            d.remove_nulls();
        }
        Ok(d)
    }

    /// The output object for source object `r`, importing it if this is the
    /// first time it is met.
    fn map_ref(&mut self, source: usize, r: ObjRef) -> Result<Option<ObjRef>> {
        let (doc, is_page, replaced) = {
            let s = self.sources.get_mut(source).ok_or_else(|| Error::Invalid("unknown import source".to_string()))?;
            if let Some(known) = s.map.get(&r.num) {
                return Ok(known.map(|n| ObjRef::new(n, 0)));
            }
            // An annotation of a page that is not in the output.
            let left_out = s.exclude.is_some_and(|(owners, kept)| {
                owners.get(&r.num).is_some_and(|pages| !pages.iter().any(|p| kept.contains(p)))
            });
            if left_out {
                s.map.insert(r.num, None);
                return Ok(None);
            }
            (s.doc, s.pages.contains(&r.num), s.replace.remove(&r.num))
        };
        // A page that is not in the output: no need to look at it.
        if is_page {
            if let Some(s) = self.sources.get_mut(source) {
                s.map.insert(r.num, None);
            }
            return Ok(None);
        }
        let object = match replaced {
            Some(o) => o,
            None => match doc.get(r) {
                Ok(o) => o,
                // Not something skipping the object would fix: the whole file is
                // out of reach (encrypted, unsupported filter, disk trouble).
                Err(e @ (Error::Unsupported(_) | Error::Io(_))) => return Err(e),
                // A damaged object: keep going without it and tell the user.
                Err(e) => {
                    self.unreadable_total = self.unreadable_total.saturating_add(1);
                    if self.unreadable.len() < MAX_UNREADABLE_LISTED {
                        self.unreadable.push(format!("object {} ({e})", r.num));
                    }
                    Object::Null
                }
            },
        };
        let keep = !matches!(object, Object::Null) && !is_structural(&object);
        let new = if keep { Some(self.reserve()?) } else { None };
        if let Some(s) = self.sources.get_mut(source) {
            s.map.insert(r.num, new.map(|n| n.num));
        }
        if let Some(new) = new {
            self.pending.push(Pending { source, new, object });
        }
        Ok(new)
    }

    // --- finishing --------------------------------------------------------------

    /// Warnings about objects that could not be read from the sources.
    fn unreadable_warning(&self) -> Option<Warning> {
        if self.unreadable_total == 0 {
            return None;
        }
        let listed = self.unreadable.join("; ");
        let more = self.unreadable_total.saturating_sub(self.unreadable.len());
        let tail = if more > 0 { format!("; and {more} more") } else { String::new() };
        Some(Warning(format!(
            "{} damaged object(s) in the input could not be read and were replaced with null: {listed}{tail}",
            self.unreadable_total
        )))
    }

    /// Write the cross-reference table and trailer and return the file, plus
    /// a warning if damaged input objects had to be replaced with null.
    ///
    /// `root` is the catalog. Numbers that were reserved but never written
    /// are written as the null object.
    pub fn finish(mut self, root: ObjRef, info: Option<ObjRef>) -> Result<(Vec<u8>, Vec<Warning>)> {
        let warning = self.unreadable_warning();
        if self.buf.is_empty() {
            self.write_header();
        }
        for i in 0..self.offsets.len() {
            if self.offsets.get(i).is_some_and(Option::is_none) {
                let num = u32::try_from(i + 1).unwrap_or(u32::MAX);
                self.put(ObjRef::new(num, 0), &Object::Null)?;
            }
        }
        let xref_at = u64::try_from(self.buf.len()).unwrap_or(u64::MAX);
        let size = self.offsets.len() + 1;
        // 7.5.4: one 20-byte entry per object, the free-list head first.
        let mut table = Vec::with_capacity(size.saturating_mul(20).saturating_add(64));
        table.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        table.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &self.offsets {
            let offset = offset.unwrap_or(0);
            if offset > MAX_OFFSET {
                return Err(Error::Limit("the file is too large for a classic cross-reference table".to_string()));
            }
            table.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        // 7.5.5: /Size is one more than the highest object number.
        table.extend_from_slice(format!("trailer\n<< /Size {size} /Root {} {} R", root.num, root.generation).as_bytes());
        if let Some(info) = info {
            table.extend_from_slice(format!(" /Info {} {} R", info.num, info.generation).as_bytes());
        }
        // 14.4: the file identifier, two equal strings for a new file.
        let id = file_id(&self.buf);
        table.extend_from_slice(b" /ID [");
        write_hex_string(&mut table, &id);
        table.push(b' ');
        write_hex_string(&mut table, &id);
        table.push(b']');
        table.extend_from_slice(format!(" >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
        self.buf.extend_from_slice(&table);
        Ok((self.buf, warning.into_iter().collect()))
    }
}

/// The file identifier (14.4): 16 bytes made from everything written so far.
/// Two SipHash runs with different starting words give 128 bits; the same
/// output always gets the same identifier, and a different one almost surely a
/// different one.
fn file_id(body: &[u8]) -> [u8; 16] {
    let mut first = std::hash::DefaultHasher::new();
    let mut second = std::hash::DefaultHasher::new();
    first.write(b"qingpdf-id-1");
    second.write(b"qingpdf-id-2");
    for chunk in body.chunks(1 << 16) {
        first.write(chunk);
        second.write(chunk);
    }
    first.write_usize(body.len());
    second.write_usize(body.len());
    let mut id = [0u8; 16];
    let (a, b) = id.split_at_mut(8);
    a.copy_from_slice(&first.finish().to_be_bytes());
    b.copy_from_slice(&second.finish().to_be_bytes());
    id
}

/// Objects that are part of a file's structure rather than its content, and
/// are never copied: page tree nodes, catalogs, cross-reference streams and
/// object streams. A reference to one becomes null.
fn is_structural(obj: &Object) -> bool {
    match obj.as_dict().and_then(|d| d.get_name("Type")) {
        Some(t) => ["Pages", "Page", "Catalog", "XRef", "ObjStm"].iter().any(|k| t == k),
        None => false,
    }
}

/// A destination written as a name (old `/Dests` dictionary) or a string
/// (name tree), directly or through a reference: its bytes, and whether it was
/// a name object.
fn named_key(doc: &Document, value: &Object) -> Option<(Vec<u8>, bool)> {
    let resolved;
    let value = match value {
        Object::Ref(_) => {
            resolved = doc.resolve(value).ok()?;
            &resolved
        }
        other => other,
    };
    match value {
        Object::Name(n) => Some((n.as_bytes().to_vec(), true)),
        Object::String(s) => Some((s.bytes.clone(), false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn written(obj: &Object) -> Vec<u8> {
        let mut out = Vec::new();
        write_object(&mut out, obj).unwrap();
        out
    }

    fn reparse(bytes: &[u8]) -> Object {
        Parser::new(bytes, 0).parse_object().unwrap()
    }

    #[test]
    fn names_are_escaped_where_the_spec_requires() {
        let name = |bytes: &[u8]| {
            let mut out = Vec::new();
            write_name(&mut out, &Name::new(bytes));
            String::from_utf8(out).unwrap()
        };
        assert_eq!(name(b"Type"), "/Type");
        assert_eq!(name(b"A;Name_With-Various***Characters?"), "/A;Name_With-Various***Characters?");
        assert_eq!(name(b"Lime Green"), "/Lime#20Green");
        assert_eq!(name(b"F#_Minor"), "/F#23_Minor");
        assert_eq!(name(b"paired()parentheses"), "/paired#28#29parentheses");
        assert_eq!(name(b"a/b%c<d>[e]{f}"), "/a#2Fb#25c#3Cd#3E#5Be#5D#7Bf#7D");
        assert_eq!(name(b"\t\r\n\x0c"), "/#09#0D#0A#0C");
        assert_eq!(name(&[0xE4, 0xB8, 0xAD, 0x7F, 0x01]), "/#E4#B8#AD#7F#01");
        assert_eq!(name(b""), "/");
    }

    #[test]
    fn every_byte_value_in_a_name_survives_our_parser() {
        let all: Vec<u8> = (1..=255u8).collect();
        let obj = Object::Name(Name::new(all.clone()));
        let bytes = written(&obj);
        // Only printable ASCII and `#` ever appear in the written form.
        assert!(bytes.iter().all(|&b| (0x21..=0x7E).contains(&b) || b == b'/'));
        assert_eq!(reparse(&bytes), obj);
    }

    #[test]
    fn literal_strings_escape_what_they_must() {
        let lit = |bytes: &[u8]| {
            let mut out = Vec::new();
            write_literal_string(&mut out, bytes);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(lit(b"plain text"), "(plain text)");
        assert_eq!(lit(b"a(b)c\\d"), "(a\\(b\\)c\\\\d)");
        assert_eq!(lit(b"line\r\nbreak\t"), "(line\\015\\012break\\011)");
        // Three octal digits, so a digit after an escape stays a digit.
        assert_eq!(lit(&[0x05, b'3']), "(\\0053)");
        assert_eq!(lit(&[0xFE, 0xFF, 0x4E, 0x2D]), "(\\376\\377N-)");
        assert_eq!(lit(b""), "()");
    }

    #[test]
    fn every_byte_value_in_a_string_survives_our_parser() {
        let all: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        for hex in [false, true] {
            let obj = Object::String(PdfString { bytes: all.clone(), hex });
            let bytes = written(&obj);
            if !hex {
                assert!(bytes.iter().all(|&b| (0x20..=0x7E).contains(&b)), "literal strings stay printable");
            }
            assert_eq!(reparse(&bytes), obj, "hex = {hex}");
        }
        // Empty strings and awkward ones.
        for s in [&b""[..], b"(", b")", b"\\", b"\\(", b"((()", b"\\\\)"] {
            for hex in [false, true] {
                let obj = Object::String(PdfString { bytes: s.to_vec(), hex });
                assert_eq!(reparse(&written(&obj)), obj);
            }
        }
    }

    #[test]
    fn hex_strings() {
        let mut out = Vec::new();
        write_hex_string(&mut out, &[0x4E, 0x6F, 0x00, 0xFF]);
        assert_eq!(out, b"<4E6F00FF>");
        let mut out = Vec::new();
        write_hex_string(&mut out, &[]);
        assert_eq!(out, b"<>");
    }

    #[test]
    fn reals_never_use_an_exponent_and_read_back_exactly() {
        for v in [
            0.0, -0.0, 1.0, -1.0, 0.5, 595.276, 841.89, 1e-7, 1.5e-300, 6.02e23, 1e21, -3.62, 0.1 + 0.2, f64::MAX,
            f64::MIN_POSITIVE, 123_456_789.123_456_79,
        ] {
            let text = format_real(v).unwrap();
            assert!(!text.contains(['e', 'E']), "{text}");
            assert!(text.contains('.'), "{text}: a real keeps its decimal point");
            assert!(text.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-')), "{text}");
            let back = text.parse::<f64>().unwrap();
            assert_eq!(back.to_bits(), v.to_bits(), "{text}");
            // And our own parser agrees.
            match reparse(text.as_bytes()) {
                Object::Real(r) => assert_eq!(r.to_bits(), v.to_bits(), "{text}"),
                other => panic!("{text} read back as {other:?}"),
            }
        }
        assert_eq!(format_real(595.276).unwrap(), "595.276");
        assert_eq!(format_real(2.0).unwrap(), "2.0");
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(format_real(bad), Err(Error::Invalid(_))));
            assert!(write_object(&mut Vec::new(), &Object::Real(bad)).is_err());
        }
    }

    #[test]
    fn composite_objects_round_trip() {
        let mut inner = Dict::new();
        inner.set("K", Object::Array(vec![Object::Integer(1), Object::Real(2.5), Object::Null, Object::Bool(true)]));
        inner.set("Weird Name", Object::Name(Name::new(b"v#1".to_vec())));
        let mut d = Dict::new();
        d.set("Type", Object::from("Test"));
        d.set("Ref", Object::Ref(ObjRef::new(7, 0)));
        d.set("Str", Object::String(PdfString::literal(b"(x)".to_vec())));
        d.set("Hex", Object::String(PdfString::hex(vec![1, 2, 3])));
        d.set("Inner", Object::Dict(inner));
        d.set("Neg", Object::Integer(-98));
        let obj = Object::Dict(d);
        assert_eq!(reparse(&written(&obj)), obj);
    }

    #[test]
    fn null_dictionary_values_are_left_out_and_direct_streams_refused() {
        let mut d = Dict::new();
        d.set("A", Object::Integer(1));
        let mut raw = Dict::new();
        raw.set("A", Object::Integer(1));
        let obj = Object::Dict(d);
        assert_eq!(written(&obj), b"<< /A 1 >>");
        let stream = Object::Stream(Stream { dict: raw, data: vec![] });
        assert!(matches!(write_object(&mut Vec::new(), &stream), Err(Error::Invalid(_))));
        assert!(matches!(
            write_object(&mut Vec::new(), &Object::Array(vec![stream])),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn absurd_nesting_is_a_limit_error_not_a_stack_overflow() {
        let mut obj = Object::Integer(0);
        for _ in 0..(MAX_WRITE_DEPTH + 10) {
            obj = Object::Array(vec![obj]);
        }
        assert!(matches!(write_object(&mut Vec::new(), &obj), Err(Error::Limit(_))));
    }

    #[test]
    fn a_new_file_has_the_structure_the_spec_describes() {
        let mut b = Builder::new((1, 6));
        let catalog = b.reserve().unwrap();
        let pages = b.add(&Object::Dict(Dict::from_iter([
            (Name::from("Type"), Object::from("Pages")),
            (Name::from("Kids"), Object::Array(vec![])),
            (Name::from("Count"), Object::Integer(0)),
        ])))
        .unwrap();
        let mut cat = Dict::new();
        cat.set("Type", Object::from("Catalog"));
        cat.set("Pages", Object::Ref(pages));
        b.put(catalog, &Object::Dict(cat)).unwrap();
        let (bytes, warnings) = b.finish(catalog, None).unwrap();
        assert!(warnings.is_empty());
        // 7.5.2: header, then a comment with at least four bytes >= 128.
        assert!(bytes.starts_with(b"%PDF-1.6\n%"));
        assert!(bytes[10..14].iter().all(|&b| b >= 128));
        assert!(bytes.ends_with(b"%%EOF\n"));
        // 7.5.4: every entry is exactly 20 bytes; startxref points at `xref`.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let at = text.rfind("startxref\n").unwrap();
        let xref_offset: usize = text[at + 10..].lines().next().unwrap().parse().unwrap();
        assert!(bytes[xref_offset..].starts_with(b"xref\n0 3\n"));
        let entries = &bytes[xref_offset + 9..];
        assert_eq!(&entries[..20], b"0000000000 65535 f \n");
        for n in 1..=2usize {
            let entry = &entries[n * 20..(n + 1) * 20];
            assert_eq!(entry[10], b' ');
            assert_eq!(&entry[11..16], b"00000");
            assert_eq!(&entry[16..], b" n \n");
            let off: usize = std::str::from_utf8(&entry[..10]).unwrap().parse().unwrap();
            assert!(bytes[off..].starts_with(format!("{n} 0 obj").as_bytes()));
        }
        assert!(text.contains("trailer\n<< /Size 3 /Root 1 0 R /ID [<"));
        // Our reader opens it.
        let doc = Document::from_bytes(bytes).unwrap();
        assert!(!doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 0);
        assert_eq!(doc.version(), (1, 6));
    }

    #[test]
    fn stream_length_is_the_real_raw_length_whatever_the_input_said() {
        let mut b = Builder::new((1, 4));
        let catalog = b.reserve().unwrap();
        let mut sd = Dict::new();
        sd.set("Length", Object::Ref(ObjRef::new(99, 0))); // stale indirect length
        sd.set("Foo", Object::Integer(1));
        let data = b"endstream is a word that may appear in data\r\n".to_vec();
        let stream = b.add(&Object::Stream(Stream { dict: sd, data: data.clone() })).unwrap();
        b.put(catalog, &Object::Dict(Dict::from_iter([(Name::from("S"), Object::Ref(stream))]))).unwrap();
        let (bytes, _) = b.finish(catalog, None).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(&format!("/Length {} >>\nstream\n", data.len())));
        assert_eq!(text.matches("/Length").count(), 1);
        // Read it back through the parser: same dictionary entry, same bytes.
        let pos = crate::lexer::find_bytes(&bytes, b"2 0 obj", 0).unwrap();
        let helper = crate::parser::PlainHelper::new(&bytes);
        let raw = Parser::new(&bytes, pos).parse_indirect(&helper).unwrap();
        let Object::Stream(s) = raw.into_object(&bytes) else { panic!("not a stream") };
        assert_eq!(s.data, data);
        assert_eq!(s.dict.get_int("Foo"), Some(1));
        assert_eq!(s.dict.get_int("Length"), Some(i64::try_from(data.len()).unwrap()));
    }

    #[test]
    fn new_streams_are_flate_compressed() {
        let mut b = Builder::new((1, 4));
        let catalog = b.reserve().unwrap();
        let payload = b"0 0 m 100 100 l S\n".repeat(50);
        let s = b.add_flate_stream(Dict::new(), &payload).unwrap();
        b.put(catalog, &Object::Dict(Dict::from_iter([(Name::from("S"), Object::Ref(s))]))).unwrap();
        let (bytes, _) = b.finish(catalog, None).unwrap();
        assert!(bytes.len() < payload.len());
        let doc = Document::from_bytes(bytes).unwrap();
        let Object::Stream(stream) = doc.get(s).unwrap() else { panic!("not a stream") };
        assert_eq!(stream.dict.get_name("Filter").unwrap(), &Name::from("FlateDecode"));
        assert_eq!(doc.decode_stream(&stream).unwrap(), payload);
    }

    #[test]
    fn unwritten_reserved_numbers_become_null_objects_and_double_writes_are_refused() {
        let mut b = Builder::new((1, 4));
        let catalog = b.reserve().unwrap();
        let hole = b.reserve().unwrap();
        b.put(catalog, &Object::Dict(Dict::new())).unwrap();
        assert!(b.put(catalog, &Object::Null).is_err());
        assert!(b.put(ObjRef::new(50, 0), &Object::Null).is_err());
        let (bytes, _) = b.finish(catalog, None).unwrap();
        let doc = Document::from_bytes(bytes).unwrap();
        assert_eq!(doc.get(hole).unwrap(), Object::Null);
    }

    #[test]
    fn info_goes_into_the_trailer() {
        let mut b = Builder::new((1, 4));
        let catalog = b.reserve().unwrap();
        b.put(catalog, &Object::Dict(Dict::new())).unwrap();
        let info = b.add(&Object::Dict(Dict::from_iter([(Name::from("Title"), Object::String(PdfString::literal("x")))]))).unwrap();
        let (bytes, _) = b.finish(catalog, Some(info)).unwrap();
        let doc = Document::from_bytes(bytes).unwrap();
        assert_eq!(doc.trailer().get("Info"), Some(&Object::Ref(info)));
        assert!(doc.info().unwrap().is_some());
    }
}
