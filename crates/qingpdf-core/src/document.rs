//! The public reading API: open a file, load objects on demand, walk the page
//! tree.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use crate::error::{Error, Result};
use crate::filter;
use crate::lexer::find_bytes;
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::parser::{EndstreamIndex, Parser, StreamHelper};
use crate::repair;
use crate::xref::{self, ObjStm, XrefEntry};

/// Longest chain of `n g R` references `resolve` will follow.
const MAX_REF_CHAIN: usize = 32;
/// How deep reading one object may recurse into reading others (indirect
/// stream lengths, filters held in object streams).
const MAX_NESTED_LOADS: u32 = 8;
/// Deepest page tree accepted (7.7.3).
const MAX_PAGE_TREE_DEPTH: usize = 256;
/// Cap on the total size of inherited attributes copied into pages, so a
/// hostile tree cannot multiply one huge `/Resources` across many pages.
const MAX_INHERITED_COPY_WEIGHT: usize = 1 << 26;
/// The header must start within this many bytes of the beginning of the file.
const HEADER_WINDOW: usize = 1024;
/// Page attributes a page inherits from its ancestors (7.7.3.4, Table 30).
const INHERITABLE: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// An open PDF file, held completely in memory. Objects are parsed when they
/// are first asked for.
pub struct Document {
    data: Vec<u8>,
    header_version: (u8, u8),
    trailer: Dict,
    xref: RefCell<HashMap<u32, XrefEntry>>,
    /// Decoded object streams (7.5.7), each decoded once. A stream that failed
    /// to decode is remembered as failed.
    objstms: RefCell<HashMap<u32, std::result::Result<Rc<ObjStm>, Error>>>,
    endstreams: EndstreamIndex,
    repaired: Cell<bool>,
    repair_attempted: Cell<bool>,
    /// Off while the file is being checked at open time.
    repair_allowed: Cell<bool>,
    uses_xref_streams: Cell<bool>,
    nesting: Cell<u32>,
    /// After a rebuild: the reason some object stream could not be opened
    /// (an unsupported filter). Objects that should be inside it are not in
    /// the table, so an object missing from the table is "unsupported", not
    /// "absent".
    unreadable: RefCell<Option<String>>,
}

/// One page, with the inheritable attributes of its ancestors copied in.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub obj_ref: ObjRef,
    /// The page dictionary with `/Resources`, `/MediaBox`, `/CropBox` and
    /// `/Rotate` filled in from the page tree where the page does not have its
    /// own (7.7.3.4), and without `/Parent`. `/MediaBox`, `/CropBox` and
    /// `/Rotate` are stored as direct objects.
    pub dict: Dict,
}

impl Page {
    /// `/MediaBox` as `[llx, lly, urx, ury]`, normalised so that the first
    /// corner is the lower left (7.9.5).
    pub fn media_box(&self) -> Option<[f64; 4]> {
        rectangle(self.dict.get("MediaBox"))
    }

    /// `/Rotate` in degrees clockwise: 0, 90, 180 or 270 (7.7.3.3, Table 30).
    pub fn rotate(&self) -> i64 {
        let raw = match self.dict.get("Rotate") {
            Some(Object::Integer(i)) => *i,
            Some(Object::Real(r)) if r.is_finite() => r.round() as i64,
            _ => 0,
        };
        let degrees = raw.rem_euclid(360);
        degrees - degrees % 90
    }
}

pub(crate) fn rectangle(obj: Option<&Object>) -> Option<[f64; 4]> {
    let [a, b, c, d] = obj?.as_array()? else {
        return None;
    };
    let (x0, y0, x1, y1) = (a.as_f64()?, b.as_f64()?, c.as_f64()?, d.as_f64()?);
    if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}

/// Counts how deeply loading one object has led to loading others.
struct NestingGuard<'a>(&'a Cell<u32>);

impl<'a> NestingGuard<'a> {
    fn enter(depth: &'a Cell<u32>) -> Option<Self> {
        let now = depth.get();
        if now >= MAX_NESTED_LOADS {
            return None;
        }
        depth.set(now + 1);
        Some(NestingGuard(depth))
    }
}

impl Drop for NestingGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// A copy of an error, for the object stream cache.
fn copy_error(e: &Error) -> Error {
    match e {
        Error::Io(io) => Error::Io(std::io::Error::new(io.kind(), io.to_string())),
        Error::Syntax { offset, message } => Error::Syntax { offset: *offset, message: message.clone() },
        Error::MissingObject { num, generation } => Error::MissingObject { num: *num, generation: *generation },
        Error::Unsupported(m) => Error::Unsupported(m.clone()),
        Error::Limit(m) => Error::Limit(m.clone()),
        Error::Invalid(m) => Error::Invalid(m.clone()),
    }
}

fn encrypted_error() -> Error {
    Error::Unsupported("encrypted PDF".to_string())
}

/// `1.7` as `(1, 7)`.
fn parse_version(bytes: &[u8]) -> Option<(u8, u8)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (major, minor) = text.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The version in the header line `%PDF-1.7` (7.5.2); `data` starts at `%`.
fn header_version(data: &[u8]) -> (u8, u8) {
    let Some(rest) = data.strip_prefix(b"%PDF-") else {
        return (1, 0);
    };
    let text: Vec<u8> = rest.iter().take(8).copied().take_while(|b| b.is_ascii_digit() || *b == b'.').collect();
    parse_version(&text).unwrap_or((1, 0))
}

impl Document {
    /// Read a file. The whole file is loaded into memory.
    pub fn open(path: impl AsRef<Path>) -> Result<Document> {
        let data = std::fs::read(path)?;
        Document::from_bytes(data)
    }

    /// Open a PDF held in memory. Reads the cross-reference data and the trailer
    /// only; objects are parsed when requested. If the cross-reference data is
    /// missing or wrong the file is scanned and the table rebuilt
    /// ([`Document::was_repaired`]).
    pub fn from_bytes(mut data: Vec<u8>) -> Result<Document> {
        // 7.5.2: the header is at the start of the file. Junk before it is
        // tolerated; offsets are then counted from the header.
        let window = data.get(..HEADER_WINDOW.min(data.len())).unwrap_or(&[]);
        if let Some(p) = find_bytes(window, b"%PDF-", 0) {
            data.drain(..p);
        }
        let version = header_version(&data);

        let first_error = match xref::read_xref(&data) {
            Ok(x) => {
                let doc = Document::assemble(data, version, x.entries, x.trailer, x.uses_xref_streams, false);
                doc.repair_allowed.set(false);
                let checked = doc.validate();
                doc.repair_allowed.set(true);
                match checked {
                    Ok(()) => return Ok(doc),
                    // The table is fine; a filter we cannot decode is in the
                    // way. Scanning the file again would not change that.
                    Err(e @ Error::Unsupported(_)) => return Err(e),
                    Err(e) => {
                        data = doc.data;
                        e
                    }
                }
            }
            Err(e) => e,
        };

        match repair::rebuild(&data) {
            Ok(r) => {
                let unsupported = r.unsupported;
                let doc = Document::assemble(data, version, r.entries, r.trailer, r.uses_xref_streams, true);
                *doc.unreadable.borrow_mut() = unsupported.clone();
                match doc.validate() {
                    Ok(()) => Ok(doc),
                    // The page tree is missing because it sits in an object
                    // stream we cannot open (or the table could not be read
                    // because of a filter we cannot decode): say so.
                    Err(e) => match (unsupported, &first_error) {
                        (Some(m), _) => Err(Error::Unsupported(m)),
                        (None, Error::Unsupported(m)) => Err(Error::Unsupported(m.clone())),
                        (None, _) => Err(e),
                    },
                }
            }
            Err(e) => match first_error {
                unsupported @ Error::Unsupported(_) => Err(unsupported),
                first_error => Err(Error::syntax(
                    None,
                    format!("cannot read the cross-reference data ({first_error}) and cannot rebuild it ({e})"),
                )),
            },
        }
    }

    fn assemble(
        data: Vec<u8>,
        header_version: (u8, u8),
        entries: HashMap<u32, XrefEntry>,
        trailer: Dict,
        uses_xref_streams: bool,
        repaired: bool,
    ) -> Document {
        Document {
            data,
            header_version,
            trailer,
            xref: RefCell::new(entries),
            objstms: RefCell::new(HashMap::new()),
            endstreams: EndstreamIndex::default(),
            repaired: Cell::new(repaired),
            repair_attempted: Cell::new(repaired),
            repair_allowed: Cell::new(true),
            uses_xref_streams: Cell::new(uses_xref_streams),
            nesting: Cell::new(0),
            unreadable: RefCell::new(None),
        }
    }

    /// Is the file usable: a catalog dictionary, and a page tree root that is
    /// really there if the catalog names one. Encrypted files cannot be checked
    /// (their object streams cannot be read) and are accepted.
    fn validate(&self) -> Result<()> {
        if self.is_encrypted() {
            return Ok(());
        }
        let catalog = self.catalog()?;
        if let Some(pages) = catalog.get("Pages")
            && self.resolve(pages)?.as_dict().is_none()
        {
            return Err(Error::syntax(None, "the page tree root named by the catalog is missing"));
        }
        Ok(())
    }

    /// Version from the header, or from the catalog's `/Version` if that is
    /// later (7.5.2, 7.7.2).
    pub fn version(&self) -> (u8, u8) {
        let mut version = self.header_version;
        let from_catalog = self.catalog().ok().and_then(|catalog| match catalog.get("Version") {
            Some(Object::Name(n)) => parse_version(n.as_bytes()),
            _ => None,
        });
        if let Some(v) = from_catalog {
            version = version.max(v);
        }
        version
    }

    /// The trailer dictionary (7.5.5). For a file read through cross-reference
    /// streams this is the stream dictionary without the entries that only
    /// describe the stream. After [`Document::was_repaired`] it is the trailer
    /// recovered by the scan.
    pub fn trailer(&self) -> &Dict {
        &self.trailer
    }

    /// The document catalog (7.7.2).
    pub fn catalog(&self) -> Result<Dict> {
        let root = self.trailer.get("Root").ok_or_else(|| Error::syntax(None, "the trailer has no /Root entry"))?;
        match self.resolve(root)? {
            Object::Dict(d) => Ok(d),
            _ => Err(Error::syntax(None, "/Root does not lead to a dictionary")),
        }
    }

    /// The document information dictionary (14.3.3), if there is one. Its
    /// strings are encrypted in an encrypted file, which is not supported.
    pub fn info(&self) -> Result<Option<Dict>> {
        let Some(entry) = self.trailer.get("Info") else {
            return Ok(None);
        };
        if self.is_encrypted() {
            return Err(encrypted_error());
        }
        match self.resolve(entry)? {
            Object::Dict(d) => Ok(Some(d)),
            _ => Ok(None),
        }
    }

    /// Load an indirect object. An object that does not exist (not in the
    /// cross-reference data, or free) is the null object (7.3.10), as is a
    /// compressed object missing from its stream. A reference whose generation
    /// differs from the file's is still honoured.
    pub fn get(&self, r: ObjRef) -> Result<Object> {
        match self.load(r) {
            // The cross-reference entry does not lead to this object: the
            // table is wrong. Rebuild it once and look again.
            Err(Error::MissingObject { .. }) if self.repair_now() => self.load(r),
            other => other,
        }
    }

    fn load(&self, r: ObjRef) -> Result<Object> {
        let entry = self.xref.borrow().get(&r.num).copied();
        match entry {
            // Not in the table at all, in a file where an object stream could
            // not be opened: the object may well be inside that stream.
            None => match self.unreadable.borrow().as_ref() {
                Some(why) => Err(Error::Unsupported(why.clone())),
                None => Ok(Object::Null),
            },
            Some(XrefEntry::Free) => Ok(Object::Null),
            Some(XrefEntry::InUse { offset, .. }) => self.load_at(r.num, offset),
            Some(XrefEntry::Compressed { stream_num, index }) => {
                let stm = self.object_stream(stream_num)?;
                Ok(stm.get(r.num, index)?.unwrap_or(Object::Null))
            }
        }
    }

    /// Parse the object at `offset`, which must be object `num`.
    /// `MissingObject` means "nothing like that there".
    fn load_at(&self, num: u32, offset: u64) -> Result<Object> {
        let missing = || Error::MissingObject { num, generation: 0 };
        let offset = usize::try_from(offset).map_err(|_| missing())?;
        if offset >= self.data.len() {
            return Err(missing());
        }
        let mut parser = Parser::new(&self.data, offset);
        let header = parser.parse_header().map_err(|_| missing())?;
        if header.num != num {
            return Err(missing());
        }
        let raw = parser.parse_body(header, self)?;
        Ok(raw.into_object(&self.data))
    }

    /// Rebuild the cross-reference table by scanning, once per document.
    /// Returns whether it happened now. The trailer is not replaced.
    fn repair_now(&self) -> bool {
        if self.repair_attempted.get() || !self.repair_allowed.get() {
            return false;
        }
        self.repair_attempted.set(true);
        match repair::rebuild(&self.data) {
            Ok(r) => {
                *self.xref.borrow_mut() = r.entries;
                *self.unreadable.borrow_mut() = r.unsupported;
                self.objstms.borrow_mut().clear();
                self.uses_xref_streams.set(r.uses_xref_streams);
                self.repaired.set(true);
                true
            }
            Err(_) => false,
        }
    }

    /// The decoded object stream `stream_num`, decoding it the first time.
    fn object_stream(&self, stream_num: u32) -> Result<Rc<ObjStm>> {
        if let Some(cached) = self.objstms.borrow().get(&stream_num) {
            return match cached {
                Ok(stm) => Ok(Rc::clone(stm)),
                Err(e) => Err(copy_error(e)),
            };
        }
        let result = self.read_object_stream(stream_num);
        let mut cache = self.objstms.borrow_mut();
        match result {
            Ok(stm) => {
                cache.insert(stream_num, Ok(Rc::clone(&stm)));
                Ok(stm)
            }
            Err(e) => {
                cache.insert(stream_num, Err(copy_error(&e)));
                Err(e)
            }
        }
    }

    fn read_object_stream(&self, stream_num: u32) -> Result<Rc<ObjStm>> {
        let _guard = NestingGuard::enter(&self.nesting)
            .ok_or_else(|| Error::Limit("object streams refer to each other too deeply".to_string()))?;
        let entry = self.xref.borrow().get(&stream_num).copied();
        let Some(XrefEntry::InUse { offset, .. }) = entry else {
            return Err(Error::syntax(None, format!("object stream {stream_num} is not an ordinary object")));
        };
        let Object::Stream(stream) = self.load_at(stream_num, offset)? else {
            return Err(Error::syntax(None, format!("object {stream_num} is not a stream")));
        };
        let decoded = match self.decode_stream(&stream) {
            Ok(d) => d,
            // Say where the unsupported filter was met.
            Err(Error::Unsupported(m)) if !self.is_encrypted() => {
                return Err(Error::Unsupported(format!("{m} in an object stream")));
            }
            Err(e) => return Err(e),
        };
        Ok(Rc::new(ObjStm::parse(&stream.dict, decoded)?))
    }

    /// Follow a chain of references until something that is not a reference.
    /// Objects that are not references are returned as they are (cloned). A
    /// reference to a missing object gives `Null`; a cycle or an excessively
    /// long chain is an error.
    pub fn resolve(&self, obj: &Object) -> Result<Object> {
        let Object::Ref(first) = obj else {
            return Ok(obj.clone());
        };
        let mut seen: Vec<ObjRef> = Vec::new();
        let mut current = *first;
        loop {
            if seen.contains(&current) {
                return Err(Error::Limit(format!("reference cycle through {} {} R", current.num, current.generation)));
            }
            if seen.len() >= MAX_REF_CHAIN {
                return Err(Error::Limit(format!("more than {MAX_REF_CHAIN} references in a row")));
            }
            seen.push(current);
            match self.get(current)? {
                Object::Ref(next) => current = next,
                other => return Ok(other),
            }
        }
    }

    /// Every object number that is in use, with its generation, in order.
    pub fn object_refs(&self) -> Vec<ObjRef> {
        let mut refs: Vec<ObjRef> = self
            .xref
            .borrow()
            .iter()
            .filter_map(|(&num, entry)| match entry {
                XrefEntry::Free => None,
                XrefEntry::InUse { generation, .. } => Some(ObjRef::new(num, *generation)),
                XrefEntry::Compressed { .. } => Some(ObjRef::new(num, 0)),
            })
            .collect();
        refs.sort();
        refs
    }

    /// Decode the data of a stream through its filters (7.4). Only
    /// `FlateDecode` (with predictors) is supported; any other filter gives
    /// [`Error::Unsupported`], and so does any stream of an encrypted file.
    pub fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>> {
        // Cross-reference streams are never encrypted (7.5.8.2).
        let is_xref_stream = matches!(s.dict.get("Type"), Some(Object::Name(n)) if n == "XRef");
        if self.is_encrypted() && !is_xref_stream {
            return Err(encrypted_error());
        }
        filter::decode(&s.dict, &s.data, &|o| self.resolve(o))
    }

    /// Does the trailer have an `/Encrypt` entry (7.5.5)?
    pub fn is_encrypted(&self) -> bool {
        self.trailer.contains_key("Encrypt")
    }

    /// Was the cross-reference table rebuilt by scanning the file?
    pub fn was_repaired(&self) -> bool {
        self.repaired.get()
    }

    /// Does the file locate its objects through cross-reference streams (7.5.8)?
    pub fn uses_xref_streams(&self) -> bool {
        self.uses_xref_streams.get()
    }

    /// Does the file hold objects inside object streams (7.5.7)?
    pub fn uses_object_streams(&self) -> bool {
        self.xref.borrow().values().any(|e| matches!(e, XrefEntry::Compressed { .. }))
    }

    /// The number of pages, found by walking the page tree.
    pub fn page_count(&self) -> Result<usize> {
        let mut count = 0usize;
        self.walk_page_tree(&mut |_, _, _| {
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    /// All pages in order, each with its inherited attributes (7.7.3.4).
    ///
    /// A node reachable twice (a cycle, or a shared subtree) is visited once.
    /// Entries of `/Kids` that are direct objects, missing, or neither a page
    /// nor a page tree node are skipped.
    pub fn pages(&self) -> Result<Vec<Page>> {
        let mut pages = Vec::new();
        let mut weight_used = 0usize;
        self.walk_page_tree(&mut |obj_ref, mut dict, inherited| {
            dict.remove("Parent");
            for (key, slot) in INHERITABLE.iter().zip(inherited.0.iter()) {
                if dict.contains_key(key) {
                    continue;
                }
                if let Some(value) = slot {
                    weight_used = weight_used.saturating_add(weight(value));
                    if weight_used > MAX_INHERITED_COPY_WEIGHT {
                        return Err(Error::Limit(
                            "inherited page attributes are too large to copy into every page".to_string(),
                        ));
                    }
                    dict.set(*key, (**value).clone());
                }
            }
            // The boxes and the rotation are small values: store them directly
            // so a page can be read without a document at hand.
            for key in ["MediaBox", "CropBox", "Rotate"] {
                if let Some(value) = dict.get(key) {
                    let direct = self.resolve_shallow(value);
                    dict.set(key, direct);
                }
            }
            pages.push(Page { obj_ref, dict });
            Ok(())
        })?;
        Ok(pages)
    }

    /// Resolve a reference, and the elements of an array, without failing:
    /// whatever cannot be resolved is left as it was.
    fn resolve_shallow(&self, obj: &Object) -> Object {
        let resolved = self.resolve(obj).unwrap_or_else(|_| obj.clone());
        match resolved {
            Object::Array(items) => {
                Object::Array(items.iter().map(|item| self.resolve(item).unwrap_or_else(|_| item.clone())).collect())
            }
            other => other,
        }
    }

    /// Visit every page in order with its own dictionary and the attributes
    /// inherited from above (7.7.3).
    fn walk_page_tree(&self, visit: &mut dyn FnMut(ObjRef, Dict, &Inherited) -> Result<()>) -> Result<()> {
        let catalog = self.catalog()?;
        let Some(Object::Ref(root)) = catalog.get("Pages") else {
            return Err(Error::syntax(None, "the catalog has no /Pages reference"));
        };
        let root = *root;
        let mut visited: HashSet<ObjRef> = HashSet::new();
        let mut stack: Vec<Frame> = Vec::new();
        if let Some(frame) = self.enter_node(root, &Inherited::default(), &mut visited, visit)? {
            stack.push(frame);
        }
        while let Some(top) = stack.last_mut() {
            let Some(kid) = top.kids.get(top.next) else {
                stack.pop();
                continue;
            };
            top.next += 1;
            // /Kids holds indirect references (Table 29); anything else is skipped.
            let Object::Ref(kid) = kid else {
                continue;
            };
            let kid = *kid;
            let inherited = top.inherited.clone();
            if stack.len() >= MAX_PAGE_TREE_DEPTH {
                return Err(Error::Limit(format!("page tree nested more than {MAX_PAGE_TREE_DEPTH} levels deep")));
            }
            if let Some(frame) = self.enter_node(kid, &inherited, &mut visited, visit)? {
                stack.push(frame);
            }
        }
        Ok(())
    }

    /// Load one page tree node. A page is handed to `visit` and `None` is
    /// returned; a page tree node gives the frame for walking its kids.
    fn enter_node(
        &self,
        r: ObjRef,
        parent: &Inherited,
        visited: &mut HashSet<ObjRef>,
        visit: &mut dyn FnMut(ObjRef, Dict, &Inherited) -> Result<()>,
    ) -> Result<Option<Frame>> {
        if !visited.insert(r) {
            return Ok(None);
        }
        let Some(node) = self.get(r)?.as_dict().cloned() else {
            return Ok(None);
        };
        match classify(&node) {
            NodeKind::Skip => Ok(None),
            NodeKind::Page => {
                visit(r, node, parent)?;
                Ok(None)
            }
            NodeKind::Pages => {
                let kids = match node.get("Kids") {
                    Some(kids) => match self.resolve(kids)? {
                        Object::Array(items) => items,
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                };
                Ok(Some(Frame { kids, next: 0, inherited: parent.with_overrides(&node) }))
            }
        }
    }
}

impl StreamHelper for Document {
    fn resolve_length(&self, r: ObjRef) -> Option<i64> {
        let _guard = NestingGuard::enter(&self.nesting)?;
        match self.get(r) {
            Ok(Object::Integer(n)) => Some(n),
            _ => None,
        }
    }

    fn find_endstream(&self, from: usize) -> Option<usize> {
        self.endstreams.find(&self.data, from)
    }
}

/// Inheritable attributes in force at some point of the page tree, in the
/// order of [`INHERITABLE`]. Shared, so passing them down costs nothing.
#[derive(Clone, Default)]
struct Inherited([Option<Rc<Object>>; 4]);

impl Inherited {
    /// What the kids of `node` inherit: this node's own values, over ours.
    fn with_overrides(&self, node: &Dict) -> Inherited {
        let mut out = self.clone();
        for (slot, key) in out.0.iter_mut().zip(INHERITABLE) {
            if let Some(value) = node.get(key) {
                *slot = Some(Rc::new(value.clone()));
            }
        }
        out
    }
}

/// A page tree node being walked.
struct Frame {
    kids: Vec<Object>,
    next: usize,
    inherited: Inherited,
}

enum NodeKind {
    Pages,
    Page,
    Skip,
}

/// Sort a page tree node (7.7.3.2, 7.7.3.3). `/Type` says which it is. Without
/// a `/Type`, `/Kids` makes a page tree node, and any of `/Contents`,
/// `/MediaBox`, `/Resources`, `/Parent` makes a page. A dictionary whose `/Type`
/// is something else is not part of the page tree.
fn classify(node: &Dict) -> NodeKind {
    match node.get("Type") {
        Some(Object::Name(n)) if n == "Pages" => NodeKind::Pages,
        Some(Object::Name(n)) if n == "Page" => NodeKind::Page,
        Some(Object::Name(_)) => NodeKind::Skip,
        _ if matches!(node.get("Kids"), Some(Object::Array(_) | Object::Ref(_))) => NodeKind::Pages,
        _ if ["Contents", "MediaBox", "Resources", "Parent"].iter().any(|k| node.contains_key(k)) => NodeKind::Page,
        _ => NodeKind::Skip,
    }
}

/// A rough size of an object, to bound how much is copied.
fn weight(obj: &Object) -> usize {
    match obj {
        Object::Array(items) => items.iter().fold(1usize, |acc, o| acc.saturating_add(weight(o))),
        Object::Dict(d) => d.iter().fold(1usize, |acc, (_, o)| acc.saturating_add(weight(o))),
        Object::String(s) => 1 + s.bytes.len() / 16,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::rfind_bytes;
    use crate::object::Name;
    use crate::testutil::*;

    fn open(bytes: Vec<u8>) -> Document {
        Document::from_bytes(bytes).expect("document should open")
    }

    fn rect(page: &Page) -> Option<[f64; 4]> {
        page.media_box()
    }

    // --- basics ---------------------------------------------------------------

    #[test]
    fn classic_file() {
        let doc = open(sample_pdf());
        assert_eq!(doc.version(), (1, 4));
        assert!(!doc.is_encrypted());
        assert!(!doc.was_repaired());
        assert!(!doc.uses_xref_streams());
        assert!(!doc.uses_object_streams());
        assert_eq!(doc.page_count().unwrap(), 1);
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].obj_ref, ObjRef::new(3, 0));
        assert_eq!(rect(&pages[0]), Some([0.0, 0.0, 200.0, 100.0]));
        assert_eq!(pages[0].rotate(), 0);
        assert!(!pages[0].dict.contains_key("Parent"));
        assert_eq!(doc.catalog().unwrap().get_name("Type").unwrap(), &Name::from("Catalog"));
        assert_eq!(doc.trailer().get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
        assert!(doc.info().unwrap().is_none());
        assert_eq!(doc.object_refs(), (1..=4).map(|n| ObjRef::new(n, 0)).collect::<Vec<_>>());
    }

    #[test]
    fn get_and_resolve() {
        let doc = open(sample_pdf());
        let page = doc.get(ObjRef::new(3, 0)).unwrap();
        assert_eq!(page.as_dict().unwrap().get_name("Type").unwrap(), &Name::from("Page"));
        // Stream objects come with their raw data.
        let Object::Stream(s) = doc.get(ObjRef::new(4, 0)).unwrap() else { panic!("not a stream") };
        assert_eq!(s.data, b"0 0 m 100 100 l S");
        assert_eq!(doc.decode_stream(&s).unwrap(), b"0 0 m 100 100 l S");
        // Missing and free objects are null; so are references to them.
        assert_eq!(doc.get(ObjRef::new(99, 0)).unwrap(), Object::Null);
        assert_eq!(doc.get(ObjRef::new(0, 65535)).unwrap(), Object::Null);
        assert_eq!(doc.resolve(&Object::Ref(ObjRef::new(99, 0))).unwrap(), Object::Null);
        // Non-references come back unchanged.
        assert_eq!(doc.resolve(&Object::Integer(5)).unwrap(), Object::Integer(5));
        // A reference with the wrong generation still finds the object.
        assert!(doc.get(ObjRef::new(3, 7)).unwrap().as_dict().is_some());
    }

    #[test]
    fn reference_chains_and_cycles() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(5, "6 0 R");
        b.obj(6, "7 0 R");
        b.obj(7, "(end)");
        b.obj(8, "9 0 R");
        b.obj(9, "8 0 R");
        b.obj(10, "10 0 R");
        for n in 20..=60u32 {
            b.obj(n, &format!("{} 0 R", n + 1));
        }
        let doc = open(b.finish_classic(100, "/Root 1 0 R"));
        let end = doc.resolve(&Object::Ref(ObjRef::new(5, 0))).unwrap();
        assert!(matches!(end, Object::String(_)));
        assert!(matches!(doc.resolve(&Object::Ref(ObjRef::new(8, 0))), Err(Error::Limit(_))));
        assert!(matches!(doc.resolve(&Object::Ref(ObjRef::new(10, 0))), Err(Error::Limit(_))));
        // A chain longer than the limit.
        assert!(matches!(doc.resolve(&Object::Ref(ObjRef::new(20, 0))), Err(Error::Limit(_))));
    }

    #[test]
    fn info_dictionary() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, "<< /Title (Hello) /Producer (test) >>");
        let doc = open(b.finish_classic(4, "/Root 1 0 R /Info 3 0 R"));
        let info = doc.info().unwrap().unwrap();
        assert!(info.contains_key("Title"));
        // An /Info that is not a dictionary reads as absent.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, "42");
        let doc = open(b.finish_classic(4, "/Root 1 0 R /Info 3 0 R"));
        assert!(doc.info().unwrap().is_none());
    }

    #[test]
    fn version_header_and_catalog() {
        let mut b = PdfBuilder::with_header("%PDF-1.3\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Version /1.6 >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        assert_eq!(open(b.finish_classic(3, "/Root 1 0 R")).version(), (1, 6));
        // An earlier catalog version does not lower the header's.
        let mut b = PdfBuilder::with_header("%PDF-1.7\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Version /1.4 >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        assert_eq!(open(b.finish_classic(3, "/Root 1 0 R")).version(), (1, 7));
        // No usable header at all.
        let mut b = PdfBuilder::with_header("");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        assert_eq!(open(b.finish_classic(3, "/Root 1 0 R")).version(), (1, 0));
    }

    #[test]
    fn junk_before_the_header_is_skipped() {
        let mut junk = b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n\r\n".to_vec();
        junk.extend_from_slice(&sample_pdf());
        let doc = open(junk);
        assert!(!doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(Document::from_bytes(Vec::new()).is_err());
        assert!(Document::from_bytes(b"hello world".to_vec()).is_err());
        assert!(Document::from_bytes(vec![0u8; 5000]).is_err());
        assert!(Document::from_bytes(b"%PDF-1.4\n".to_vec()).is_err());
    }

    #[test]
    fn open_reads_a_file() {
        let dir = std::env::temp_dir().join(format!("qingpdf-open-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.pdf");
        std::fs::write(&path, sample_pdf()).unwrap();
        let doc = Document::open(&path).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
        assert!(matches!(Document::open(dir.join("missing.pdf")), Err(Error::Io(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_reads_a_file_with_chinese_and_spaces_in_its_path() {
        let dir = std::env::temp_dir().join(format!("qingpdf-open-test-中文 目录-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("测试 文件.pdf");
        std::fs::write(&path, sample_pdf()).unwrap();
        let doc = Document::open(&path).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn more_than_a_window_of_junk_after_eof_is_repaired() {
        let mut pdf = sample_pdf();
        pdf.extend(std::iter::repeat_n(b' ', 10_000));
        let doc = open(pdf);
        assert!(doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn direct_page_tree_root_is_an_error() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages << /Type /Pages /Kids [] /Count 0 >> >>");
        let doc = open(b.finish_classic(2, "/Root 1 0 R"));
        assert!(matches!(doc.page_count(), Err(Error::Syntax { .. })));
    }

    #[test]
    fn wide_page_tree_is_fast() {
        // 20,000 pages in one flat node, all inheriting a direct /Resources.
        let n = 20_000u32;
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..n).map(|i| format!("{} 0 R ", 3 + i)).collect();
        b.obj(
            2,
            &format!("<< /Type /Pages /Kids [{kids}] /Count {n} /MediaBox [0 0 10 10] /Resources << /Font << /F1 1 0 R >> >> >>"),
        );
        for i in 0..n {
            b.obj(3 + i, "<< /Type /Page /Parent 2 0 R >>");
        }
        let doc = open(b.finish_classic(3 + n, "/Root 1 0 R"));
        let start = std::time::Instant::now();
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), n as usize);
        assert_eq!(doc.page_count().unwrap(), n as usize);
        assert!(pages.iter().all(|p| p.media_box() == Some([0.0, 0.0, 10.0, 10.0])));
        assert!(start.elapsed().as_secs() < 10, "took {:?}", start.elapsed());
    }

    // --- cross-reference structures -------------------------------------------

    #[test]
    fn incremental_update() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let x1 = b.classic_xref(4, "/Root 1 0 R");
        b.startxref(x1);
        // Update: a new MediaBox for the page, a new second page.
        let o3 = b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 500 600] >>");
        let o2 = b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        let o4 = b.obj(4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 20] >>");
        let x2 = b.len();
        b.raw(format!("xref\n2 3\n{o2:010} 00000 n \n{o3:010} 00000 n \n{o4:010} 00000 n \n").as_bytes());
        b.raw(format!("trailer << /Size 5 /Root 1 0 R /Prev {x1} >>\n").as_bytes());
        b.startxref(x2);
        let doc = open(b.finish());
        assert!(!doc.was_repaired());
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(rect(&pages[0]), Some([0.0, 0.0, 500.0, 600.0]));
        assert_eq!(rect(&pages[1]), Some([0.0, 0.0, 10.0, 20.0]));
    }

    #[test]
    fn cross_reference_stream_and_object_stream() {
        let doc = open(sample_objstm_pdf());
        assert!(!doc.was_repaired());
        assert!(doc.uses_xref_streams());
        assert!(doc.uses_object_streams());
        assert_eq!(doc.version(), (1, 5));
        assert_eq!(doc.page_count().unwrap(), 1);
        let pages = doc.pages().unwrap();
        assert_eq!(rect(&pages[0]), Some([0.0, 0.0, 300.0, 400.0]));
        // The trailer is the xref stream dictionary minus the stream-only keys.
        assert!(doc.trailer().contains_key("Root"));
        assert!(!doc.trailer().contains_key("W"));
        // Compressed objects have generation 0 and are listed.
        let refs = doc.object_refs();
        assert!(refs.contains(&ObjRef::new(1, 0)) && refs.contains(&ObjRef::new(3, 0)));
        assert!(!refs.contains(&ObjRef::new(0, 0)));
    }

    #[test]
    fn object_streams_are_decoded_once() {
        // Opening checks the catalog and page tree root, which live in the
        // object stream: it is decoded then and never again.
        let doc = open(sample_objstm_pdf());
        assert_eq!(doc.objstms.borrow().len(), 1);
        let first = Rc::clone(doc.objstms.borrow().values().next().unwrap().as_ref().unwrap());
        for _ in 0..3 {
            doc.get(ObjRef::new(1, 0)).unwrap();
            doc.get(ObjRef::new(2, 0)).unwrap();
            doc.get(ObjRef::new(3, 0)).unwrap();
        }
        assert_eq!(doc.objstms.borrow().len(), 1);
        let again = Rc::clone(doc.objstms.borrow().values().next().unwrap().as_ref().unwrap());
        assert!(Rc::ptr_eq(&first, &again));
    }

    #[test]
    fn hybrid_file() {
        // Object 3 (Outlines) is hidden from old readers: free in the table,
        // compressed in the xref stream named by /XRefStm.
        let mut b = PdfBuilder::with_header("%PDF-1.5\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let header = "3 0 ";
        b.flate_stream_obj(
            5,
            &format!("/Type /ObjStm /N 1 /First {}", header.len()),
            format!("{header}<< /Type /Outlines /Count 0 >>").as_bytes(),
        );
        let o1 = b.offset_of(1);
        let o2 = b.offset_of(2);
        let o5 = b.offset_of(5);
        let main = b.len();
        b.raw(format!("xref\n0 6\n0000000000 65535 f \n{o1:010} 00000 n \n{o2:010} 00000 n \n0000000000 65535 f \n0000000000 00000 f \n{o5:010} 00000 n \n").as_bytes());
        b.raw(b"trailer << /Size 7 /Root 1 0 R >>\n");
        b.startxref(main);
        let stm = b.len();
        b.stream_obj(6, "/Type /XRef /Size 7 /W [1 2 1] /Index [3 1]", &[2, 0, 5, 0]);
        let upd = b.len();
        b.raw(format!("xref\n0 0\ntrailer << /Size 7 /Root 1 0 R /Prev {main} /XRefStm {stm} >>\n").as_bytes());
        b.startxref(upd);
        let doc = open(b.finish());
        assert!(!doc.was_repaired());
        assert!(doc.uses_xref_streams());
        assert!(doc.uses_object_streams());
        let outlines = doc.get(ObjRef::new(3, 0)).unwrap();
        assert_eq!(outlines.as_dict().unwrap().get_name("Type").unwrap(), &Name::from("Outlines"));
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    // --- repair ---------------------------------------------------------------

    fn replace(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let at = find_bytes(bytes, from, 0).expect("pattern not found");
        let mut out = bytes[..at].to_vec();
        out.extend_from_slice(to);
        out.extend_from_slice(&bytes[at + from.len()..]);
        out
    }

    #[test]
    fn broken_startxref_is_repaired() {
        let pdf = sample_pdf();
        let at = rfind_bytes(&pdf, b"startxref\n", 0).unwrap() + "startxref\n".len();
        let end = at + pdf[at..].iter().position(|&b| b == b'\n').unwrap();
        let mut broken = pdf[..at].to_vec();
        broken.extend_from_slice(b"99999");
        broken.extend_from_slice(&pdf[end..]);
        let doc = open(broken);
        assert!(doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
        assert_eq!(rect(&doc.pages().unwrap()[0]), Some([0.0, 0.0, 200.0, 100.0]));
        assert_eq!(doc.trailer().get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
    }

    #[test]
    fn missing_startxref_and_xref_table_are_repaired() {
        let pdf = sample_pdf();
        let at = find_bytes(&pdf, b"xref\n0 5", 0).unwrap();
        let doc = open(pdf[..at].to_vec());
        assert!(doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
        // The trailer is found by locating the catalog.
        assert_eq!(doc.trailer().get("Root"), Some(&Object::Ref(ObjRef::new(1, 0))));
    }

    #[test]
    fn truncated_after_the_objects_is_repaired() {
        let pdf = sample_pdf();
        // Cut in the middle of the trailer.
        let at = find_bytes(&pdf, b"trailer", 0).unwrap() + 10;
        let doc = open(pdf[..at].to_vec());
        assert!(doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn wrong_xref_offsets_trigger_a_rebuild_on_demand() {
        // Every offset is shifted by 7 bytes: the Root cannot be found at open.
        let mut b = PdfBuilder::new();
        let o1 = b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>") + 7;
        let o2 = b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>") + 7;
        let o3 = b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>") + 7;
        let table = b.len();
        b.raw(
            format!("xref\n0 4\n0000000000 65535 f \n{o1:010} 00000 n \n{o2:010} 00000 n \n{o3:010} 00000 n \n")
                .as_bytes(),
        );
        b.raw(b"trailer << /Size 4 /Root 1 0 R >>\n");
        b.startxref(table);
        let doc = open(b.finish());
        assert!(doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn a_bad_offset_found_later_is_repaired_lazily() {
        // The catalog and page tree are fine, one page object's offset is wrong.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>");
        let o3 = b.offset_of(3);
        let pdf = b.finish_classic(4, "/Root 1 0 R");
        let bad = format!("{:010} 00000 n ", o3 + 3);
        let good = format!("{o3:010} 00000 n ");
        let pdf = replace(&pdf, good.as_bytes(), bad.as_bytes());
        let doc = open(pdf);
        assert!(!doc.was_repaired(), "nothing wrong with the parts loaded at open");
        assert_eq!(doc.page_count().unwrap(), 1);
        assert!(doc.was_repaired());
        assert_eq!(rect(&doc.pages().unwrap()[0]), Some([0.0, 0.0, 5.0, 5.0]));
    }

    #[test]
    fn prev_cycle_ends_in_repair_not_a_hang() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>");
        let o: Vec<usize> = (1..=3).map(|n| b.offset_of(n)).collect();
        let x = b.len();
        b.raw(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
                o[0], o[1], o[2]
            )
            .as_bytes(),
        );
        b.raw(format!("trailer << /Size 4 /Root 1 0 R /Prev {x} >>\n").as_bytes());
        b.startxref(x);
        let doc = open(b.finish());
        assert!(doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn xref_stream_file_with_damaged_startxref_is_repaired() {
        let pdf = sample_objstm_pdf();
        let at = rfind_bytes(&pdf, b"startxref", 0).unwrap();
        let mut broken = pdf[..at].to_vec();
        broken.extend_from_slice(b"startxref\n7\n%%EOF\n");
        let doc = open(broken);
        assert!(doc.was_repaired());
        assert!(doc.uses_xref_streams());
        assert!(doc.uses_object_streams());
        assert_eq!(doc.page_count().unwrap(), 1);
        assert_eq!(rect(&doc.pages().unwrap()[0]), Some([0.0, 0.0, 300.0, 400.0]));
    }

    // --- streams --------------------------------------------------------------

    #[test]
    fn wrong_stream_length_falls_back_to_endstream() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.raw_obj(3, b"3 0 obj\n<< /Length 3 >>\nstream\nthe real data is longer\nendstream\nendobj\n");
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        let Object::Stream(s) = doc.get(ObjRef::new(3, 0)).unwrap() else { panic!() };
        assert_eq!(s.data, b"the real data is longer");
        // And a length beyond the end of the file.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.raw_obj(3, b"3 0 obj\n<< /Length 99999 >>\nstream\nshort\nendstream\nendobj\n");
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        let Object::Stream(s) = doc.get(ObjRef::new(3, 0)).unwrap() else { panic!() };
        assert_eq!(s.data, b"short");
    }

    #[test]
    fn indirect_stream_length() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        // The data contains "endstream", so only the real length gives the right answer.
        b.raw_obj(3, b"3 0 obj\n<< /Length 4 0 R >>\nstream\nab endstream cd\nendstream\nendobj\n");
        b.obj(4, "16");
        let doc = open(b.finish_classic(5, "/Root 1 0 R"));
        let Object::Stream(s) = doc.get(ObjRef::new(3, 0)).unwrap() else { panic!() };
        assert_eq!(s.data, b"ab endstream cd\n");
        // A /Length that points at its own stream cannot be resolved; no hang.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.raw_obj(3, b"3 0 obj\n<< /Length 3 0 R >>\nstream\nxyz\nendstream\nendobj\n");
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        let Object::Stream(s) = doc.get(ObjRef::new(3, 0)).unwrap() else { panic!() };
        assert_eq!(s.data, b"xyz");
    }

    #[test]
    fn flate_stream_through_the_document() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.flate_stream_obj(3, "", b"hello flate");
        // Filter and DecodeParms through references.
        b.obj(4, "[/FlateDecode]");
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&[2, 1, 2, 3, 2, 1, 1, 1], 6);
        b.stream_obj(5, "/Filter 4 0 R /DecodeParms << /Predictor 12 /Columns 3 >>", &packed);
        b.stream_obj(6, "/Filter /LZWDecode", b"xx");
        let doc = open(b.finish_classic(7, "/Root 1 0 R"));
        let Object::Stream(s) = doc.get(ObjRef::new(3, 0)).unwrap() else { panic!() };
        assert_eq!(doc.decode_stream(&s).unwrap(), b"hello flate");
        let Object::Stream(s) = doc.get(ObjRef::new(5, 0)).unwrap() else { panic!() };
        assert_eq!(doc.decode_stream(&s).unwrap(), [1, 2, 3, 2, 3, 4]);
        // Other filters: Unsupported, raw bytes remain on the stream.
        let Object::Stream(s) = doc.get(ObjRef::new(6, 0)).unwrap() else { panic!() };
        assert!(matches!(doc.decode_stream(&s), Err(Error::Unsupported(_))));
        assert_eq!(s.data, b"xx");
    }

    #[test]
    fn deep_nesting_in_an_object_is_a_limit_error() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, &format!("{}{}", "[".repeat(1000), "]".repeat(1000)));
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        assert!(matches!(doc.get(ObjRef::new(3, 0)), Err(Error::Limit(_))));
        // The rest of the document is unaffected.
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    // --- page tree --------------------------------------------------------------

    #[test]
    fn inherited_attributes() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 4 /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 9 0 R >> >> /Rotate 90 /CropBox [10 10 600 780] >>",
        );
        // Inner node overrides MediaBox and passes the rest down.
        b.obj(3, "<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R] /Count 2 /MediaBox [0 0 300 300] >>");
        // Inner node with nothing of its own.
        b.obj(4, "<< /Type /Pages /Parent 2 0 R /Kids [7 0 R 8 0 R] /Count 2 >>");
        // 5: inherits everything from 3 and 2.
        b.obj(5, "<< /Type /Page /Parent 3 0 R >>");
        // 6: overrides Rotate and Resources, MediaBox is an indirect reference.
        b.obj(6, "<< /Type /Page /Parent 3 0 R /Rotate -90 /Resources << /ProcSet [/PDF] >> /MediaBox 10 0 R >>");
        // 7: its own MediaBox with reversed corners, inherits the rest from 2.
        b.obj(7, "<< /Type /Page /Parent 4 0 R /MediaBox [200 100 0 0] >>");
        // 8: Rotate is an indirect reference, resources are an indirect reference too.
        b.obj(8, "<< /Type /Page /Parent 4 0 R /Rotate 11 0 R /Resources 12 0 R >>");
        b.obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.obj(10, "[0 0 11 0 R 13 0 R]");
        b.obj(11, "180");
        b.obj(12, "<< /Font << /F2 9 0 R >> >>");
        b.obj(13, "50.5");
        let doc = open(b.finish_classic(14, "/Root 1 0 R"));
        let pages = doc.pages().unwrap();
        assert_eq!(doc.page_count().unwrap(), 4);
        let refs: Vec<u32> = pages.iter().map(|p| p.obj_ref.num).collect();
        assert_eq!(refs, vec![5, 6, 7, 8]);

        // Page 5.
        assert_eq!(rect(&pages[0]), Some([0.0, 0.0, 300.0, 300.0]));
        assert_eq!(pages[0].rotate(), 90);
        assert!(pages[0].dict.get("Resources").unwrap().as_dict().unwrap().contains_key("Font"));
        assert_eq!(
            pages[0].dict.get("CropBox"),
            Some(&Object::Array(vec![
                Object::Integer(10),
                Object::Integer(10),
                Object::Integer(600),
                Object::Integer(780)
            ]))
        );
        assert!(!pages[0].dict.contains_key("Parent"));

        // Page 6: own values win; /Rotate -90 reads as 270; indirect MediaBox
        // (with an indirect element) is stored directly.
        assert_eq!(pages[1].rotate(), 270);
        assert!(pages[1].dict.get("Resources").unwrap().as_dict().unwrap().contains_key("ProcSet"));
        assert_eq!(rect(&pages[1]), Some([0.0, 0.0, 180.0, 50.5]));
        assert!(matches!(pages[1].dict.get("MediaBox"), Some(Object::Array(_))));

        // Page 7: reversed corners are normalised; the dict keeps what was written.
        assert_eq!(rect(&pages[2]), Some([0.0, 0.0, 200.0, 100.0]));
        assert_eq!(pages[2].rotate(), 90);

        // Page 8: indirect Rotate becomes direct, indirect Resources stay as written.
        assert_eq!(pages[3].dict.get("Rotate"), Some(&Object::Integer(180)));
        assert_eq!(pages[3].rotate(), 180);
        assert_eq!(pages[3].dict.get("Resources"), Some(&Object::Ref(ObjRef::new(12, 0))));
        // MediaBox comes from the root through node 4.
        assert_eq!(rect(&pages[3]), Some([0.0, 0.0, 612.0, 792.0]));
    }

    #[test]
    fn rotate_is_normalised() {
        let page = |value: Object| {
            let mut dict = Dict::new();
            dict.set("Rotate", value);
            Page { obj_ref: ObjRef::new(1, 0), dict }
        };
        assert_eq!(page(Object::Integer(0)).rotate(), 0);
        assert_eq!(page(Object::Integer(90)).rotate(), 90);
        assert_eq!(page(Object::Integer(360)).rotate(), 0);
        assert_eq!(page(Object::Integer(450)).rotate(), 90);
        assert_eq!(page(Object::Integer(-90)).rotate(), 270);
        assert_eq!(page(Object::Integer(-450)).rotate(), 270);
        assert_eq!(page(Object::Real(180.0)).rotate(), 180);
        assert_eq!(page(Object::Integer(100)).rotate(), 90);
        assert_eq!(page(Object::Integer(i64::MIN)).rotate() % 90, 0);
        assert_eq!(page(Object::from("x")).rotate(), 0);
        assert_eq!(Page { obj_ref: ObjRef::new(1, 0), dict: Dict::new() }.rotate(), 0);
    }

    #[test]
    fn media_box_edge_cases() {
        let page = |value: Object| {
            let mut dict = Dict::new();
            dict.set("MediaBox", value);
            Page { obj_ref: ObjRef::new(1, 0), dict }
        };
        let nums = |v: &[f64]| Object::Array(v.iter().map(|&x| Object::Real(x)).collect());
        assert_eq!(page(nums(&[0.0, 0.0, 10.0, 20.0])).media_box(), Some([0.0, 0.0, 10.0, 20.0]));
        assert_eq!(page(nums(&[10.0, 20.0, 0.0, 0.0])).media_box(), Some([0.0, 0.0, 10.0, 20.0]));
        assert_eq!(page(nums(&[0.0, 0.0, 10.0])).media_box(), None);
        assert_eq!(page(nums(&[0.0, 0.0, 10.0, 20.0, 30.0])).media_box(), None);
        assert_eq!(page(nums(&[0.0, 0.0, f64::NAN, 20.0])).media_box(), None);
        assert_eq!(page(Object::Array(vec![Object::from("a"); 4])).media_box(), None);
        assert_eq!(page(Object::Integer(5)).media_box(), None);
        assert_eq!(Page { obj_ref: ObjRef::new(1, 0), dict: Dict::new() }.media_box(), None);
    }

    #[test]
    fn page_tree_cycles_and_sharing() {
        // Pages node 2 lists itself and its parent loop; page 3 is listed twice.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [4 0 R 3 0 R 2 0 R 3 0 R 5 0 R] /Count 2 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>");
        // 4 -> 2 (a loop back to the root), with its own page 6.
        b.obj(4, "<< /Type /Pages /Parent 2 0 R /Kids [2 0 R 6 0 R 4 0 R] /Count 1 >>");
        b.obj(5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 2 2] >>");
        b.obj(6, "<< /Type /Page /Parent 4 0 R /MediaBox [0 0 3 3] >>");
        let doc = open(b.finish_classic(7, "/Root 1 0 R"));
        let refs: Vec<u32> = doc.pages().unwrap().iter().map(|p| p.obj_ref.num).collect();
        assert_eq!(refs, vec![6, 3, 5]);
        assert_eq!(doc.page_count().unwrap(), 3);
    }

    #[test]
    fn page_tree_billion_laughs_is_cut_off() {
        // Ten levels, each listing the next level's node ten times: 10^10 pages
        // if shared nodes were expanded.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        for level in 0..10u32 {
            let kids = format!("{} 0 R ", level + 3).repeat(10);
            b.obj(2 + level, &format!("<< /Type /Pages /Kids [{kids}] /Count 1 >>"));
        }
        b.obj(12, "<< /Type /Page /MediaBox [0 0 1 1] >>");
        let doc = open(b.finish_classic(13, "/Root 1 0 R"));
        assert_eq!(doc.pages().unwrap().len(), 1);
    }

    #[test]
    fn page_tree_depth_limit() {
        fn chain(depth: u32) -> Vec<u8> {
            let mut b = PdfBuilder::new();
            b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
            for level in 0..depth {
                b.obj(2 + level, &format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", level + 3));
            }
            b.obj(2 + depth, "<< /Type /Page /MediaBox [0 0 1 1] >>");
            b.finish_classic(3 + depth, "/Root 1 0 R")
        }
        assert_eq!(open(chain(100)).page_count().unwrap(), 1);
        let deep = open(chain(400));
        assert!(matches!(deep.page_count(), Err(Error::Limit(_))));
        assert!(matches!(deep.pages(), Err(Error::Limit(_))));
    }

    #[test]
    fn odd_page_tree_nodes_are_skipped_or_classified() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(
            2,
            "<< /Type /Pages /Count 6 /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R 99 0 R << /Type /Page >> 8 0 R 9 0 R 10 0 R] >>",
        );
        b.obj(3, "<< /Type /Font >>"); // some other kind of dictionary
        b.obj(4, "<< /MediaBox [0 0 1 1] >>"); // no /Type, has page keys: a page
        b.obj(5, "(a string)"); // not a dictionary
        b.obj(6, "<< /Kids [8 0 R] >>"); // no /Type but /Kids: a node
        b.obj(7, "<< >>"); // nothing: skipped
        b.obj(8, "<< /Type /Page /MediaBox [0 0 2 2] >>");
        b.obj(9, "null");
        b.obj(10, "<< /Type /Pages >>"); // no kids at all
        let doc = open(b.finish_classic(11, "/Root 1 0 R"));
        let refs: Vec<u32> = doc.pages().unwrap().iter().map(|p| p.obj_ref.num).collect();
        assert_eq!(refs, vec![4, 8]);
    }

    #[test]
    fn kids_as_an_indirect_array() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids 4 0 R /Count 1 >>");
        b.obj(3, "<< /Type /Page /MediaBox [0 0 1 1] >>");
        b.obj(4, "[3 0 R]");
        let doc = open(b.finish_classic(5, "/Root 1 0 R"));
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn missing_page_tree_is_an_error_not_a_panic() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog >>");
        let doc = open(b.finish_classic(2, "/Root 1 0 R"));
        assert!(doc.page_count().is_err());
        assert!(doc.pages().is_err());
    }

    #[test]
    fn corrupt_page_object_is_an_error() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /MediaBox [0 0 1 1] /Contents (oops >>");
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        assert!(matches!(doc.pages(), Err(Error::Syntax { .. })));
        assert!(matches!(doc.page_count(), Err(Error::Syntax { .. })));
    }

    // --- encryption ---------------------------------------------------------------

    #[test]
    fn encrypted_trailer_is_detected() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /MediaBox [0 0 1 1] /Contents 4 0 R >>");
        b.stream_obj(4, "", b"encrypted bytes");
        b.obj(5, "<< /Filter /Standard /V 1 /R 2 /O (x) /U (y) /P -4 >>");
        b.obj(6, "<< /Title (scrambled) >>");
        let doc = open(b.finish_classic(7, "/Root 1 0 R /Encrypt 5 0 R /Info 6 0 R /ID [<aa> <bb>]"));
        assert!(doc.is_encrypted());
        assert!(!doc.was_repaired());
        // Structure is readable; anything needing decryption is Unsupported.
        assert_eq!(doc.page_count().unwrap(), 1);
        let Object::Stream(s) = doc.get(ObjRef::new(4, 0)).unwrap() else { panic!() };
        match doc.decode_stream(&s) {
            Err(Error::Unsupported(m)) => assert_eq!(m, "encrypted PDF"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(doc.info(), Err(Error::Unsupported(_))));
    }

    #[test]
    fn encrypted_file_with_unreadable_object_streams_still_opens() {
        // The catalog lives in an (encrypted, so unreadable) object stream.
        let mut b = PdfBuilder::new();
        let o5 = b.stream_obj(5, "/Type /ObjStm /N 1 /First 4", b"\x01\x02\x03\x04garbage");
        let o6 = b.obj(6, "<< /Filter /Standard >>");
        let x = b.len();
        let row = |t: u8, a: usize| [t, (a >> 8) as u8, a as u8, 0];
        let rows: Vec<u8> = [row(2, 5), row(1, o5), row(1, o6), row(1, x)].concat();
        b.stream_obj(7, "/Type /XRef /Size 8 /W [1 2 1] /Index [1 1 5 3] /Root 1 0 R /Encrypt 6 0 R", &rows);
        b.startxref(x);
        let doc = open(b.finish());
        assert!(doc.is_encrypted());
        assert!(matches!(doc.page_count(), Err(Error::Unsupported(_))));
    }

    #[test]
    fn encryption_is_still_noticed_when_the_trailer_is_lost() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, "<< /Filter /Standard /V 1 /R 2 /O <0011223344> /U <5566778899> /P -4 >>");
        // No xref, no trailer, no startxref.
        let doc = open(b.finish());
        assert!(doc.was_repaired());
        assert!(doc.is_encrypted());
        assert_eq!(doc.trailer().get("Encrypt"), Some(&Object::Ref(ObjRef::new(3, 0))));
    }

    #[test]
    fn object_stream_whose_filter_is_inside_itself_does_not_recurse_forever() {
        let mut b = PdfBuilder::with_header("%PDF-1.5\n");
        let o1 = b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o2 = b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        // Object stream 5 is filtered by /Filter 7 0 R, and object 7 is in object stream 5.
        let o5 = b.stream_obj(5, "/Type /ObjStm /N 1 /First 4 /Filter 7 0 R", b"7 0 /FlateDecode");
        let x = b.len();
        let row = |t: u8, a: usize| [t, (a >> 8) as u8, a as u8, 0];
        let rows: Vec<u8> = [row(1, o1), row(1, o2), row(2, 5), row(1, o5), row(1, x)].concat();
        b.stream_obj(6, "/Type /XRef /Size 8 /W [1 2 1] /Index [1 2 7 1 5 2] /Root 1 0 R", &rows);
        b.startxref(x);
        let doc = open(b.finish());
        // The rows above are in /Index order: objects 1,2 / 7 / 5,6.
        assert!(matches!(doc.get(ObjRef::new(7, 0)), Err(Error::Limit(_)) | Err(Error::Syntax { .. })));
        // Asking again gives the remembered failure, not another long recursion.
        assert!(doc.get(ObjRef::new(7, 0)).is_err());
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    // --- unsupported filters hiding the structure ---------------------------------------

    /// 1.5-style file whose object stream (holding the catalog, page tree and
    /// page) is compressed with a filter this layer does not know.
    fn brotli_objstm_pdf(brotli_xref_stream: bool) -> Vec<u8> {
        let mut b = PdfBuilder::with_header("%PDF-2.0\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        let header = "1 0 2 40 3 90 ";
        let o5 = b.stream_obj(
            5,
            &format!("/Type /ObjStm /N 3 /First {} /Filter /BrotliDecode", header.len()),
            b"not brotli, but nobody will look",
        );
        let o4 = b.stream_obj(4, "", b"0 0 m 1 1 l S");
        let x = b.len();
        let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
        let rows: Vec<u8> =
            [row(0, 0, 255), row(2, 5, 0), row(2, 5, 1), row(2, 5, 2), row(1, o4, 0), row(1, o5, 0), row(1, x, 0)]
                .concat();
        let (filter, data) = if brotli_xref_stream {
            ("/Filter /BrotliDecode", b"xref bytes in some other format".to_vec())
        } else {
            ("", rows)
        };
        b.stream_obj(6, &format!("/Type /XRef /Size 7 /W [1 2 1] /Root 1 0 R {filter}"), &data);
        b.startxref(x);
        b.finish()
    }

    #[test]
    fn unsupported_filter_in_an_object_stream_is_unsupported_not_damaged() {
        // The table reads fine; the page tree is inside the unreadable stream.
        match Document::from_bytes(brotli_objstm_pdf(false)) {
            Err(Error::Unsupported(m)) => assert!(m.contains("BrotliDecode filter"), "{m}"),
            Err(e) => panic!("wrong kind of error: {e:?}"),
            Ok(_) => panic!("should not open"),
        }
    }

    #[test]
    fn unsupported_filter_on_the_xref_stream_and_object_streams_is_unsupported() {
        // Neither the table nor the objects can be read, so the file is
        // scanned; the catalog and page tree are still out of reach.
        match Document::from_bytes(brotli_objstm_pdf(true)) {
            Err(Error::Unsupported(m)) => assert!(m.contains("BrotliDecode filter"), "{m}"),
            Err(e) => panic!("wrong kind of error: {e:?}"),
            Ok(_) => panic!("should not open"),
        }
    }

    #[test]
    fn rebuilt_file_with_plain_catalog_but_unreadable_page_tree_is_unsupported() {
        // Like the Brotli prototype files: the catalog is an ordinary object,
        // the page tree is in an object stream with an unknown filter and the
        // cross-reference stream cannot be read either.
        let mut b = PdfBuilder::with_header("%PDF-2.0\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.stream_obj(5, "/Type /ObjStm /N 2 /First 8 /Filter /BrotliDecode", b"whatever bytes");
        let x = b.len();
        b.stream_obj(6, "/Type /XRef /Size 7 /W [1 2 1] /Root 1 0 R /Filter /BrotliDecode", b"opaque");
        b.startxref(x);
        match Document::from_bytes(b.finish()) {
            Err(Error::Unsupported(m)) => assert!(m.contains("BrotliDecode filter"), "{m}"),
            other => panic!("{:?}", other.map(|_| "opened")),
        }
    }

    #[test]
    fn object_missing_from_a_rebuilt_table_is_unsupported_when_an_object_stream_was_unreadable() {
        // Catalog and page tree are plain objects, so the file opens after the
        // rebuild; a page that lives in the unreadable object stream is not
        // quietly "null".
        let mut b = PdfBuilder::with_header("%PDF-2.0\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.stream_obj(5, "/Type /ObjStm /N 1 /First 4 /Filter /BrotliDecode", b"opaque");
        let x = b.len();
        b.stream_obj(6, "/Type /XRef /Size 7 /W [1 2 1] /Root 1 0 R /Filter /BrotliDecode", b"opaque");
        b.startxref(x);
        let doc = open(b.finish());
        assert!(doc.was_repaired());
        assert!(doc.get(ObjRef::new(1, 0)).is_ok());
        match doc.get(ObjRef::new(3, 0)) {
            Err(Error::Unsupported(m)) => assert!(m.contains("BrotliDecode filter"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(doc.pages(), Err(Error::Unsupported(_))));
    }

    // --- robustness ---------------------------------------------------------------

    #[test]
    fn every_truncated_prefix_is_safe() {
        for pdf in [sample_pdf(), sample_objstm_pdf()] {
            for len in 0..=pdf.len() {
                let prefix = pdf[..len].to_vec();
                if let Ok(doc) = Document::from_bytes(prefix) {
                    // Whatever opened must also be usable without panicking.
                    let _ = doc.version();
                    let _ = doc.page_count();
                    let _ = doc.pages();
                    let _ = doc.info();
                    for r in doc.object_refs() {
                        if let Ok(Object::Stream(s)) = doc.get(r) {
                            let _ = doc.decode_stream(&s);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_single_byte_corruption_is_safe() {
        for pdf in [sample_pdf(), sample_objstm_pdf()] {
            for i in 0..pdf.len() {
                for value in [0u8, b'0', b'(', b'<', b'\n', 0xFF] {
                    let mut data = pdf.clone();
                    data[i] = value;
                    if let Ok(doc) = Document::from_bytes(data) {
                        let _ = doc.pages();
                        for r in doc.object_refs() {
                            let _ = doc.get(r);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn weight_counts_nested_content() {
        let mut d = Dict::new();
        d.set("A", Object::Array(vec![Object::Integer(1); 10]));
        d.set("B", Object::Dict(Dict::new()));
        assert_eq!(weight(&Object::Dict(d)), 1 + 11 + 1);
    }
}
