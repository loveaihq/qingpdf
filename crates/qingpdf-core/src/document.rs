//! The public reading API: open a file, load objects on demand, walk the page
//! tree.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use crate::error::{Error, Result};
use crate::filter::{self, DecodeBudget, MAX_DECODED_SIZE, MAX_OBJSTM_DECODED};
use crate::lexer::find_bytes;
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::parser::{EndstreamIndex, Parser, StreamHelper};
use crate::repair::{self, Unopened};
use crate::xref::{self, ObjStm, XrefEntry, XrefTable};

/// Longest chain of `n g R` references `resolve` will follow.
const MAX_REF_CHAIN: usize = 32;
/// How deep reading one object may recurse into reading others (indirect
/// stream lengths, filters held in object streams).
const MAX_NESTED_LOADS: u32 = 8;
/// Deepest page tree accepted (7.7.3).
const MAX_PAGE_TREE_DEPTH: usize = 256;
/// Most pages one document may have once shared subtrees and repeated pages
/// are expanded.
pub const MAX_PAGES: usize = 1_000_000;
/// Most page tree entries looked at in all. Repeats are expanded, so a tree
/// that lists a node ten times at each of ten levels would otherwise be
/// ten billion visits.
const MAX_PAGE_TREE_VISITS: usize = 2_000_000;
/// Most times a page may be listed again after its first listing, all pages
/// together. A tiny file can name one page a million times.
const MAX_REPEATED_PAGES: usize = 10_000;
/// Cap on the bytes of the page dictionaries that are met again (a page
/// listed several times, each listing written out in full), so a hostile tree
/// cannot make one huge page count many times over.
const MAX_REPEATED_BYTES: usize = 64 * 1024 * 1024;
/// Decoded object streams are kept for reuse up to this many bytes in all; the
/// one used longest ago is dropped first. A file whose objects are spread over
/// object streams in an order that visits them all again and again (bookmarks
/// interleaved over 250 streams, say) needs all of them at once, up to this
/// much; with more than this it ends in a limit error. Larger would make a
/// document (the cache, the file, and the output being built) cost more than
/// 200 MB.
const OBJSTM_CACHE_BYTES: usize = 96 * 1024 * 1024;
/// The header must start within this many bytes of the beginning of the file.
const HEADER_WINDOW: usize = 1024;
/// Page attributes a page inherits from its ancestors (7.7.3.4, Table 30).
const INHERITABLE: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];
const RESOURCES: usize = 0;
const MEDIA_BOX: usize = 1;
const CROP_BOX: usize = 2;
const ROTATE: usize = 3;

/// An open PDF file, held completely in memory. Objects are parsed when they
/// are first asked for.
pub struct Document {
    data: Vec<u8>,
    header_version: (u8, u8),
    trailer: Dict,
    xref: RefCell<XrefTable>,
    /// Decoded object streams (7.5.7), each decoded once while it stays in
    /// the cache. A stream that failed to decode is remembered as failed.
    objstms: RefCell<ObjStmCache>,
    /// What is left of the decoding work this document may cause.
    budget: DecodeBudget,
    /// Parsed objects kept for reuse, when asked for ([`Document::cache_objects`]).
    cache: RefCell<ObjectCache>,
    endstreams: EndstreamIndex,
    repaired: Cell<bool>,
    repair_attempted: Cell<bool>,
    /// Off while the file is being checked at open time.
    repair_allowed: Cell<bool>,
    uses_xref_streams: Cell<bool>,
    nesting: Cell<u32>,
    /// After a rebuild: the reason some object stream could not be opened (an
    /// unsupported filter, or too big). Objects that should be inside it are
    /// not in the table, so an object missing from the table is "unsupported"
    /// or "over the limit", not "absent".
    unreadable: RefCell<Unopened>,
}

/// One inheritable value as written, and with the references in it followed
/// (for `/MediaBox`, `/CropBox` and `/Rotate`, which may be written
/// indirectly, also inside the array).
#[derive(Debug, Clone, PartialEq)]
struct Attr {
    raw: Object,
    resolved: Object,
}

/// The inheritable attributes in force at some point of the page tree, in the
/// order of [`INHERITABLE`]. Shared: a page tree node's values are made once
/// and every page below it points at them, so a big `/Resources` on the root
/// is not copied into every page.
#[derive(Debug, Clone, Default, PartialEq)]
struct Inherited([Option<Rc<Attr>>; 4]);

impl Inherited {
    /// What the kids of a node inherit: its own values over ours.
    fn with_overrides(&self, own: &[Option<Rc<Attr>>; 4]) -> Inherited {
        let mut out = self.clone();
        for (slot, mine) in out.0.iter_mut().zip(own) {
            if mine.is_some() {
                slot.clone_from(mine);
            }
        }
        out
    }
}

/// What a walk of the page tree may spend.
#[derive(Debug, Clone, Copy)]
struct TreeLimits {
    pages: usize,
    visits: usize,
    repeated_pages: usize,
    repeated_bytes: usize,
}

const TREE_LIMITS: TreeLimits = TreeLimits {
    pages: MAX_PAGES,
    visits: MAX_PAGE_TREE_VISITS,
    repeated_pages: MAX_REPEATED_PAGES,
    repeated_bytes: MAX_REPEATED_BYTES,
};

/// A page inherited value, with something to tell two of them apart.
#[derive(Debug, Clone, Copy)]
pub struct SharedAttr<'a> {
    /// The value as written in the page tree node it comes from.
    pub object: &'a Object,
    /// Same for every page that inherits this very value (an address that
    /// stays put as long as the pages are alive).
    pub id: usize,
}

/// The parts of a page that are looked at often, with the page tree's
/// inheritance applied and references followed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Boxes {
    media: Option<[f64; 4]>,
    crop: Option<[f64; 4]>,
    rotate: i64,
}

/// One page.
///
/// The page keeps its own dictionary and, apart from that, points at the
/// values it inherits from the page tree (7.7.3.4): the accessors
/// [`Page::resources`], [`Page::media_box`], [`Page::crop_box`] and
/// [`Page::rotate`] give the value in force, the page's own or inherited.
/// Nothing inherited is copied into the page, so a document in which a
/// thousand pages inherit one large `/Resources` holds it once.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub obj_ref: ObjRef,
    /// The page's own dictionary, without `/Parent`. It has only what the
    /// page itself says: inheritable entries (`/Resources`, `/MediaBox`,
    /// `/CropBox`, `/Rotate`) that come from the page tree are not in it.
    /// Shared between the entries of a page that is listed more than once.
    pub dict: Rc<Dict>,
    inherited: Inherited,
    boxes: Boxes,
}

impl Page {
    /// A page that inherits nothing, from its own dictionary alone (references
    /// in `/MediaBox`, `/CropBox` and `/Rotate` are not followed).
    pub fn new(obj_ref: ObjRef, dict: Dict) -> Page {
        let boxes = Boxes {
            media: rectangle(dict.get("MediaBox")),
            crop: rectangle(dict.get("CropBox")),
            rotate: rotation(dict.get("Rotate")),
        };
        Page { obj_ref, dict: Rc::new(dict), inherited: Inherited::default(), boxes }
    }

    /// `/Resources` in force: the page's own, else the nearest ancestor's, as
    /// written (a dictionary or a reference to one). 7.7.3.4.
    pub fn resources(&self) -> Option<&Object> {
        self.dict.get("Resources").or_else(|| self.inherited_slot(RESOURCES).map(|a| &a.raw))
    }

    /// `/MediaBox` as `[llx, lly, urx, ury]`, normalised so that the first
    /// corner is the lower left (7.9.5). `None` if there is none or it is not
    /// four numbers.
    pub fn media_box(&self) -> Option<[f64; 4]> {
        self.boxes.media
    }

    /// `/CropBox`, the same way. 7.7.3.3 Table 30.
    pub fn crop_box(&self) -> Option<[f64; 4]> {
        self.boxes.crop
    }

    /// `/Rotate` in degrees clockwise: 0, 90, 180 or 270 (7.7.3.3, Table 30).
    pub fn rotate(&self) -> i64 {
        self.boxes.rotate
    }

    fn inherited_slot(&self, index: usize) -> Option<&Rc<Attr>> {
        self.inherited.0.get(index)?.as_ref()
    }

    /// The value of an inheritable attribute (`/Resources`, `/MediaBox`,
    /// `/CropBox`, `/Rotate`) that this page gets from the page tree because
    /// its own dictionary does not have it, if there is one.
    pub fn inherited(&self, key: &str) -> Option<SharedAttr<'_>> {
        if self.dict.contains_key(key) {
            return None;
        }
        let index = INHERITABLE.iter().position(|k| *k == key)?;
        let attr = self.inherited_slot(index)?;
        Some(SharedAttr { object: &attr.raw, id: Rc::as_ptr(attr) as usize })
    }

    /// Like [`Page::inherited`], with the references followed (only
    /// meaningful for `/MediaBox`, `/CropBox` and `/Rotate`).
    pub fn inherited_resolved(&self, key: &str) -> Option<&Object> {
        if self.dict.contains_key(key) {
            return None;
        }
        let index = INHERITABLE.iter().position(|k| *k == key)?;
        self.inherited_slot(index).map(|a| &a.resolved)
    }
}

/// `/Rotate` as 0, 90, 180 or 270: the value modulo 360, rounded down to a
/// multiple of 90. Missing or not a number: 0. A real is rounded.
fn rotation(obj: Option<&Object>) -> i64 {
    let raw = match obj {
        Some(Object::Integer(i)) => *i,
        Some(Object::Real(r)) if r.is_finite() => r.round() as i64,
        _ => 0,
    };
    let degrees = raw.rem_euclid(360);
    degrees - degrees % 90
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

/// Decoded object streams that are kept for reuse: at most
/// [`OBJSTM_CACHE_BYTES`] of decoded data, the oldest dropped first. A failure
/// is kept too (it weighs next to nothing), so a broken stream is not decoded
/// again for every object that lives in it.
struct ObjStmCache {
    map: HashMap<u32, CachedObjStm>,
    /// Object stream numbers by when they were last used (a counter that goes
    /// up), the one used longest ago first.
    recency: BTreeMap<u64, u32>,
    clock: u64,
    bytes: usize,
    /// How many bytes it may hold.
    capacity: usize,
}

struct CachedObjStm {
    result: std::result::Result<Rc<ObjStm>, Error>,
    weight: usize,
    used: u64,
}

impl Default for ObjStmCache {
    fn default() -> Self {
        ObjStmCache::with_capacity(OBJSTM_CACHE_BYTES)
    }
}

impl ObjStmCache {
    fn with_capacity(capacity: usize) -> ObjStmCache {
        ObjStmCache { map: HashMap::new(), recency: BTreeMap::new(), clock: 0, bytes: 0, capacity }
    }

    /// The cached result, which counts as used now.
    fn get(&mut self, num: u32) -> Option<Result<Rc<ObjStm>>> {
        let entry = self.map.get_mut(&num)?;
        self.recency.remove(&entry.used);
        self.clock += 1;
        entry.used = self.clock;
        self.recency.insert(self.clock, num);
        Some(match &entry.result {
            Ok(stm) => Ok(Rc::clone(stm)),
            Err(e) => Err(copy_error(e)),
        })
    }

    fn insert(&mut self, num: u32, result: &Result<Rc<ObjStm>>) {
        let weight = match result {
            Ok(stm) => stm.weight(),
            Err(_) => 64,
        };
        // Make room by dropping what was used longest ago.
        while self.bytes.saturating_add(weight) > self.capacity {
            let Some((_, oldest)) = self.recency.pop_first() else {
                break;
            };
            if let Some(gone) = self.map.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(gone.weight);
            }
        }
        let stored = match result {
            Ok(stm) => Ok(Rc::clone(stm)),
            Err(e) => Err(copy_error(e)),
        };
        self.clock += 1;
        if let Some(old) = self.map.insert(num, CachedObjStm { result: stored, weight, used: self.clock }) {
            // Cannot happen (callers look first), but keep the books straight.
            self.bytes = self.bytes.saturating_sub(old.weight);
            self.recency.remove(&old.used);
        }
        self.recency.insert(self.clock, num);
        self.bytes = self.bytes.saturating_add(weight);
    }

    fn clear(&mut self) {
        *self = ObjStmCache::with_capacity(self.capacity);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// Parsed objects kept by object number, until `budget` bytes are used.
#[derive(Default)]
struct ObjectCache {
    map: HashMap<u32, Object>,
    bytes: usize,
    budget: usize,
}

impl ObjectCache {
    fn remember(&mut self, num: u32, object: &Object) {
        if self.budget == 0 {
            return;
        }
        let size = object.approx_size();
        if self.bytes.saturating_add(size) <= self.budget {
            self.bytes += size;
            self.map.insert(num, object.clone());
        }
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
        // One budget for everything the document makes the decoder do, from
        // the first cross-reference stream on.
        let budget = DecodeBudget::default();

        let first_error = match xref::read_xref_budgeted(&data, &budget) {
            Ok(x) => {
                let doc = Document::assemble(data, version, x.entries, x.trailer, x.uses_xref_streams, false, budget);
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
                        return Document::open_after_rebuild(data, version, doc.budget, e);
                    }
                }
            }
            Err(e) => e,
        };
        Document::open_after_rebuild(data, version, budget, first_error)
    }

    /// The cross-reference data could not be used (`first_error` says why):
    /// scan the file for the objects instead.
    fn open_after_rebuild(
        data: Vec<u8>,
        version: (u8, u8),
        budget: DecodeBudget,
        first_error: Error,
    ) -> Result<Document> {
        match repair::rebuild_budgeted(&data, &budget) {
            Ok(r) => {
                let unopened = r.unopened;
                let doc = Document::assemble(data, version, r.entries, r.trailer, r.uses_xref_streams, true, budget);
                *doc.unreadable.borrow_mut() = unopened.clone();
                match doc.validate() {
                    Ok(()) => Ok(doc),
                    // The page tree is missing because it sits in an object
                    // stream we cannot open (or the table could not be read
                    // because of a filter we cannot decode): say so.
                    Err(e) => match (unopened, &first_error) {
                        (Unopened { unsupported: Some(m), .. }, _) => Err(Error::Unsupported(m)),
                        (_, Error::Unsupported(m)) => Err(Error::Unsupported(m.clone())),
                        (Unopened { limit: Some(m), .. }, _) => Err(Error::Limit(m)),
                        _ => Err(e),
                    },
                }
            }
            Err(e) => match first_error {
                unsupported @ Error::Unsupported(_) => Err(unsupported),
                // A limit is the answer, not "the file is damaged".
                limit @ Error::Limit(_) => Err(limit),
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
        entries: XrefTable,
        trailer: Dict,
        uses_xref_streams: bool,
        repaired: bool,
        budget: DecodeBudget,
    ) -> Document {
        Document {
            data,
            header_version,
            trailer,
            xref: RefCell::new(entries),
            objstms: RefCell::new(ObjStmCache::default()),
            budget,
            cache: RefCell::new(ObjectCache::default()),
            endstreams: EndstreamIndex::default(),
            repaired: Cell::new(repaired),
            repair_attempted: Cell::new(repaired),
            repair_allowed: Cell::new(true),
            uses_xref_streams: Cell::new(uses_xref_streams),
            nesting: Cell::new(0),
            unreadable: RefCell::new(Unopened::default()),
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
        if let Some(hit) = self.cache.borrow().map.get(&r.num) {
            return Ok(hit.clone());
        }
        let loaded = match self.load(r) {
            // The cross-reference entry does not lead to this object: the
            // table is wrong. Rebuild it once and look again.
            Err(Error::MissingObject { .. }) if self.repair_now() => self.load(r),
            other => other,
        };
        if let Ok(object) = &loaded {
            self.cache.borrow_mut().remember(r.num, object);
        }
        loaded
    }

    /// Keep the objects that are loaded from now on, up to about `max_bytes`
    /// of them (those that came first; no later object replaces them), so that
    /// reading the same ones again costs a copy instead of a parse. Meant for
    /// a job that goes over the same objects many times, such as writing a
    /// document out in many pieces. 0 stops it and lets go of what is kept.
    pub fn cache_objects(&self, max_bytes: usize) {
        let mut cache = self.cache.borrow_mut();
        *cache = ObjectCache { budget: max_bytes, ..ObjectCache::default() };
    }

    fn load(&self, r: ObjRef) -> Result<Object> {
        let entry = self.xref.borrow().get(r.num);
        match entry {
            // Not in the table at all, in a file where an object stream could
            // not be opened: the object may well be inside that stream.
            None => {
                let why = self.unreadable.borrow();
                match (&why.unsupported, &why.limit) {
                    (Some(m), _) => Err(Error::Unsupported(m.clone())),
                    (None, Some(m)) => Err(Error::Limit(m.clone())),
                    (None, None) => Ok(Object::Null),
                }
            }
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
        match repair::rebuild_budgeted(&self.data, &self.budget) {
            Ok(r) => {
                *self.xref.borrow_mut() = r.entries;
                *self.unreadable.borrow_mut() = r.unopened;
                self.objstms.borrow_mut().clear();
                let budget = self.cache.borrow().budget;
                *self.cache.borrow_mut() = ObjectCache { budget, ..ObjectCache::default() };
                self.uses_xref_streams.set(r.uses_xref_streams);
                self.repaired.set(true);
                true
            }
            Err(_) => false,
        }
    }

    /// The decoded object stream `stream_num`, decoding it the first time (and
    /// again after the cache has let it go).
    fn object_stream(&self, stream_num: u32) -> Result<Rc<ObjStm>> {
        if let Some(cached) = self.objstms.borrow_mut().get(stream_num) {
            return cached;
        }
        let result = self.read_object_stream(stream_num);
        self.objstms.borrow_mut().insert(stream_num, &result);
        result
    }

    fn read_object_stream(&self, stream_num: u32) -> Result<Rc<ObjStm>> {
        let _guard = NestingGuard::enter(&self.nesting)
            .ok_or_else(|| Error::Limit("object streams refer to each other too deeply".to_string()))?;
        let entry = self.xref.borrow().get(stream_num);
        let Some(XrefEntry::InUse { offset, .. }) = entry else {
            return Err(Error::syntax(None, format!("object stream {stream_num} is not an ordinary object")));
        };
        let Object::Stream(stream) = self.load_at(stream_num, offset)? else {
            return Err(Error::syntax(None, format!("object {stream_num} is not a stream")));
        };
        let decoded = match self.decode_stream_limited(&stream, MAX_OBJSTM_DECODED) {
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
        self.xref
            .borrow()
            .iter()
            .filter_map(|(num, entry)| match entry {
                XrefEntry::Free => None,
                XrefEntry::InUse { generation, .. } => Some(ObjRef::new(num, generation)),
                XrefEntry::Compressed { .. } => Some(ObjRef::new(num, 0)),
            })
            .collect()
    }

    /// Decode the data of a stream through its filters (7.4). Only
    /// `FlateDecode` (with predictors) is supported; any other filter gives
    /// [`Error::Unsupported`], and so does any stream of an encrypted file.
    /// All the decoding a document does counts against one budget: past it,
    /// this is [`Error::Limit`].
    pub fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>> {
        self.decode_stream_limited(s, MAX_DECODED_SIZE)
    }

    fn decode_stream_limited(&self, s: &Stream, limit: usize) -> Result<Vec<u8>> {
        // Cross-reference streams are never encrypted (7.5.8.2).
        let is_xref_stream = matches!(s.dict.get("Type"), Some(Object::Name(n)) if n == "XRef");
        if self.is_encrypted() && !is_xref_stream {
            return Err(encrypted_error());
        }
        filter::decode_with_limit(&s.dict, &s.data, &|o| self.resolve(o), limit, 0, &self.budget)
    }

    /// Spend what is left of the decoding budget and forget the decoded object
    /// streams, so that the next one needed has to be decoded (and cannot be).
    #[cfg(test)]
    pub(crate) fn use_up_the_decoding_budget_for_test(&self) {
        self.budget.charge_for_test(self.budget.remaining());
        self.objstms.borrow_mut().clear();
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
        self.xref.borrow().has_compressed()
    }

    /// The number of pages, found by walking the page tree.
    pub fn page_count(&self) -> Result<usize> {
        let mut count = 0usize;
        self.walk_page_tree(TREE_LIMITS, &mut |_, _, _| {
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    /// All pages in order, each with its inherited attributes (7.7.3.4).
    ///
    /// A node reachable more than once (a subtree or a page listed under two
    /// parents, or twice in one `/Kids`) is expanded every time, as the other
    /// readers do; only a true cycle, a node that is its own ancestor, is cut.
    /// At most [`MAX_PAGES`] pages and a bounded number of tree entries are
    /// looked at, so a tree that lists a node many times at every level ends
    /// in [`Error::Limit`]. Entries of `/Kids` that are direct objects,
    /// missing, or neither a page nor a page tree node are skipped.
    pub fn pages(&self) -> Result<Vec<Page>> {
        self.pages_within(TREE_LIMITS)
    }

    fn pages_within(&self, limits: TreeLimits) -> Result<Vec<Page>> {
        let mut pages = Vec::new();
        self.walk_page_tree(limits, &mut |obj_ref, node, inherited| {
            let boxes = self.page_boxes(&node.dict, inherited);
            pages.push(Page { obj_ref, dict: Rc::clone(&node.dict), inherited: inherited.clone(), boxes });
            Ok(())
        })?;
        Ok(pages)
    }

    /// The boxes and the rotation of a page: its own values, else the
    /// inherited ones, with references followed.
    fn page_boxes(&self, own: &Dict, inherited: &Inherited) -> Boxes {
        let value = |key: &str, index: usize| -> Option<Object> {
            match own.get(key) {
                Some(v) => Some(self.resolve_shallow(v)),
                None => inherited.0.get(index)?.as_ref().map(|a| a.resolved.clone()),
            }
        };
        Boxes {
            media: rectangle(value("MediaBox", MEDIA_BOX).as_ref()),
            crop: rectangle(value("CropBox", CROP_BOX).as_ref()),
            rotate: rotation(value("Rotate", ROTATE).as_ref()),
        }
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

    /// Load one page tree node, once per walk.
    fn load_node(&self, r: ObjRef) -> Result<Option<Rc<Node>>> {
        let mut dict = match self.get(r)? {
            Object::Dict(d) => d,
            Object::Stream(s) => s.dict,
            _ => return Ok(None),
        };
        let kind = classify(&dict);
        dict.remove("Parent");
        let node = match kind {
            NodeKind::Skip => return Ok(None),
            NodeKind::Page => {
                let size = dict.approx_size();
                Node { kind, dict: Rc::new(dict), kids: Rc::new(Vec::new()), own: Default::default(), size, seen: Cell::new(false) }
            }
            NodeKind::Pages => {
                let kids = match dict.remove("Kids") {
                    Some(kids) => match self.resolve(&kids)? {
                        Object::Array(items) => items,
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                };
                // Only the inheritable values matter of a page tree node; they
                // move out of the dictionary into shared cells.
                let mut own: [Option<Rc<Attr>>; 4] = Default::default();
                for (slot, key) in own.iter_mut().zip(INHERITABLE) {
                    if let Some(raw) = dict.remove(key) {
                        let resolved = if key == "Resources" { raw.clone() } else { self.resolve_shallow(&raw) };
                        *slot = Some(Rc::new(Attr { raw, resolved }));
                    }
                }
                Node { kind, dict: Rc::new(Dict::new()), kids: Rc::new(kids), own, size: 0, seen: Cell::new(false) }
            }
        };
        Ok(Some(Rc::new(node)))
    }

    /// Visit every page in order with its own dictionary and the attributes
    /// inherited from above (7.7.3). Shared subtrees and repeated pages are
    /// visited every time; a node that is its own ancestor is skipped.
    fn walk_page_tree(
        &self,
        limits: TreeLimits,
        visit: &mut dyn FnMut(ObjRef, &Node, &Inherited) -> Result<()>,
    ) -> Result<()> {
        let catalog = self.catalog()?;
        let Some(Object::Ref(root)) = catalog.get("Pages") else {
            return Err(Error::syntax(None, "the catalog has no /Pages reference"));
        };
        let root = *root;
        let mut walk = TreeWalk::new(limits);
        let mut stack: Vec<Frame> = Vec::new();
        if let Some(frame) = self.enter_node(root, &Inherited::default(), &mut walk, visit)? {
            stack.push(frame);
        }
        while let Some(top) = stack.last_mut() {
            let Some(kid) = top.kids.get(top.next) else {
                walk.on_path.remove(&top.node);
                stack.pop();
                continue;
            };
            top.next += 1;
            // /Kids holds indirect references (Table 29); anything else is skipped.
            let Object::Ref(kid) = kid else {
                walk.count_visit()?;
                continue;
            };
            let kid = *kid;
            let inherited = top.inherited.clone();
            if stack.len() >= MAX_PAGE_TREE_DEPTH {
                return Err(Error::Limit(format!("page tree nested more than {MAX_PAGE_TREE_DEPTH} levels deep")));
            }
            if let Some(frame) = self.enter_node(kid, &inherited, &mut walk, visit)? {
                stack.push(frame);
            }
        }
        Ok(())
    }

    /// Look at one page tree entry. A page is handed to `visit` and `None` is
    /// returned; a page tree node gives the frame for walking its kids.
    fn enter_node(
        &self,
        r: ObjRef,
        parent: &Inherited,
        walk: &mut TreeWalk,
        visit: &mut dyn FnMut(ObjRef, &Node, &Inherited) -> Result<()>,
    ) -> Result<Option<Frame>> {
        walk.count_visit()?;
        let node = match walk.nodes.get(&r) {
            Some(known) => Rc::clone(known),
            None => {
                let Some(loaded) = self.load_node(r)? else {
                    return Ok(None);
                };
                walk.nodes.insert(r, Rc::clone(&loaded));
                loaded
            }
        };
        match node.kind {
            NodeKind::Skip => Ok(None),
            NodeKind::Page => {
                walk.pages += 1;
                if walk.pages > walk.limits.pages {
                    return Err(Error::Limit(format!("more than {} pages in the page tree", walk.limits.pages)));
                }
                // A page met again counts against how often and how much (its
                // bytes, every time): a tiny file cannot name one page a million
                // times, nor one big page a few hundred times.
                if node.seen.replace(true) {
                    walk.repeated_pages += 1;
                    walk.repeated_bytes = walk.repeated_bytes.saturating_add(node.size);
                    if walk.repeated_pages > walk.limits.repeated_pages {
                        return Err(Error::Limit(format!(
                            "pages are listed again more than {} times in the page tree",
                            walk.limits.repeated_pages
                        )));
                    }
                    if walk.repeated_bytes > walk.limits.repeated_bytes {
                        return Err(Error::Limit("pages listed over and over in the page tree are too large".to_string()));
                    }
                }
                visit(r, &node, parent)?;
                Ok(None)
            }
            NodeKind::Pages => {
                // A node already on the way down to here is a cycle.
                if !walk.on_path.insert(r) {
                    return Ok(None);
                }
                Ok(Some(Frame { node: r, kids: Rc::clone(&node.kids), next: 0, inherited: parent.with_overrides(&node.own) }))
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

/// A page tree node, parsed once per walk.
struct Node {
    kind: NodeKind,
    /// Without `/Parent`, and for a page tree node without `/Kids`.
    dict: Rc<Dict>,
    kids: Rc<Vec<Object>>,
    /// The inheritable values this node sets (for a page tree node).
    own: [Option<Rc<Attr>>; 4],
    /// About how many bytes the page dictionary takes.
    size: usize,
    /// Has a page with this node's dictionary been handed out already?
    seen: Cell<bool>,
}

/// What a page tree walk keeps track of.
struct TreeWalk {
    limits: TreeLimits,
    nodes: HashMap<ObjRef, Rc<Node>>,
    /// The page tree nodes between the root and where the walk is.
    on_path: HashSet<ObjRef>,
    visits: usize,
    pages: usize,
    repeated_pages: usize,
    repeated_bytes: usize,
}

impl TreeWalk {
    fn new(limits: TreeLimits) -> TreeWalk {
        TreeWalk {
            limits,
            nodes: HashMap::new(),
            on_path: HashSet::new(),
            visits: 0,
            pages: 0,
            repeated_pages: 0,
            repeated_bytes: 0,
        }
    }

    fn count_visit(&mut self) -> Result<()> {
        self.visits += 1;
        if self.visits > self.limits.visits {
            return Err(Error::Limit(format!("more than {} entries in the page tree", self.limits.visits)));
        }
        Ok(())
    }
}

/// A page tree node being walked.
struct Frame {
    node: ObjRef,
    kids: Rc<Vec<Object>>,
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
        let first = doc.objstms.borrow_mut().get(5).unwrap().unwrap();
        for _ in 0..3 {
            doc.get(ObjRef::new(1, 0)).unwrap();
            doc.get(ObjRef::new(2, 0)).unwrap();
            doc.get(ObjRef::new(3, 0)).unwrap();
        }
        assert_eq!(doc.objstms.borrow().len(), 1);
        let again = doc.objstms.borrow_mut().get(5).unwrap().unwrap();
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

        // Page 5: everything is inherited, and nothing is copied into the page.
        assert_eq!(rect(&pages[0]), Some([0.0, 0.0, 300.0, 300.0]));
        assert_eq!(pages[0].rotate(), 90);
        assert!(pages[0].resources().unwrap().as_dict().unwrap().contains_key("Font"));
        assert_eq!(pages[0].crop_box(), Some([10.0, 10.0, 600.0, 780.0]));
        assert!(!pages[0].dict.contains_key("Parent"));
        for key in ["Resources", "MediaBox", "CropBox", "Rotate"] {
            assert!(!pages[0].dict.contains_key(key), "{key} is inherited, not copied");
            assert!(pages[0].inherited(key).is_some(), "{key}");
        }
        // Pages 5 and 6 share the root's /Resources (page 6 has its own) and
        // the root's /CropBox: the same value, not two copies.
        assert_eq!(pages[0].inherited("CropBox").unwrap().id, pages[1].inherited("CropBox").unwrap().id);
        assert!(pages[1].inherited("Resources").is_none());
        assert_eq!(pages[0].inherited("Resources").unwrap().id, pages[2].inherited("Resources").unwrap().id);

        // Page 6: own values win; /Rotate -90 reads as 270; the MediaBox is an
        // indirect reference (with an indirect element), stored as written but
        // read through.
        assert_eq!(pages[1].rotate(), 270);
        assert!(pages[1].resources().unwrap().as_dict().unwrap().contains_key("ProcSet"));
        assert_eq!(rect(&pages[1]), Some([0.0, 0.0, 180.0, 50.5]));
        assert!(matches!(pages[1].dict.get("MediaBox"), Some(Object::Ref(_))));

        // Page 7: reversed corners are normalised; the dict keeps what was written.
        assert_eq!(rect(&pages[2]), Some([0.0, 0.0, 200.0, 100.0]));
        assert_eq!(pages[2].rotate(), 90);

        // Page 8: an indirect /Rotate is read through, an indirect /Resources
        // stays a reference.
        assert_eq!(pages[3].dict.get("Rotate"), Some(&Object::Ref(ObjRef::new(11, 0))));
        assert_eq!(pages[3].rotate(), 180);
        assert_eq!(pages[3].resources(), Some(&Object::Ref(ObjRef::new(12, 0))));
        // MediaBox comes from the root through node 4.
        assert_eq!(rect(&pages[3]), Some([0.0, 0.0, 612.0, 792.0]));
    }

    #[test]
    fn rotate_is_normalised() {
        let page = |value: Object| {
            let mut dict = Dict::new();
            dict.set("Rotate", value);
            Page::new(ObjRef::new(1, 0), dict)
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
        assert_eq!(Page::new(ObjRef::new(1, 0), Dict::new()).rotate(), 0);
    }

    #[test]
    fn media_box_edge_cases() {
        let page = |value: Object| {
            let mut dict = Dict::new();
            dict.set("MediaBox", value);
            Page::new(ObjRef::new(1, 0), dict)
        };
        let nums = |v: &[f64]| Object::Array(v.iter().map(|&x| Object::Real(x)).collect());
        assert_eq!(page(nums(&[0.0, 0.0, 10.0, 20.0])).media_box(), Some([0.0, 0.0, 10.0, 20.0]));
        assert_eq!(page(nums(&[10.0, 20.0, 0.0, 0.0])).media_box(), Some([0.0, 0.0, 10.0, 20.0]));
        assert_eq!(page(nums(&[0.0, 0.0, 10.0])).media_box(), None);
        assert_eq!(page(nums(&[0.0, 0.0, 10.0, 20.0, 30.0])).media_box(), None);
        assert_eq!(page(nums(&[0.0, 0.0, f64::NAN, 20.0])).media_box(), None);
        assert_eq!(page(Object::Array(vec![Object::from("a"); 4])).media_box(), None);
        assert_eq!(page(Object::Integer(5)).media_box(), None);
        assert_eq!(Page::new(ObjRef::new(1, 0), Dict::new()).media_box(), None);
    }

    #[test]
    fn page_tree_cycles_are_cut_and_repeats_are_expanded() {
        // Pages node 2 lists itself and a node that loops back to it; page 3
        // is listed twice. Only the loops are cut; the repeated page counts
        // every time, as in the other readers.
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
        assert_eq!(refs, vec![6, 3, 3, 5]);
        assert_eq!(doc.page_count().unwrap(), 4);
    }

    #[test]
    fn a_shared_subtree_is_expanded_under_each_parent_with_its_own_inheritance() {
        // Node 4 (one page, no box of its own) hangs under both 3 and 5; the
        // page gets the box of the parent it is reached through.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
        b.obj(3, "<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 100 100] /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Pages /Kids [6 0 R] /Count 1 /Parent 3 0 R >>");
        b.obj(5, "<< /Type /Pages /Kids [4 0 R] /Count 1 /MediaBox [0 0 200 200] /Parent 2 0 R >>");
        b.obj(6, "<< /Type /Page /Parent 4 0 R >>");
        let doc = open(b.finish_classic(7, "/Root 1 0 R"));
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].obj_ref, pages[1].obj_ref);
        assert_eq!(rect(&pages[0]), Some([0.0, 0.0, 100.0, 100.0]));
        assert_eq!(rect(&pages[1]), Some([0.0, 0.0, 200.0, 200.0]));
        // The page dictionary itself is shared, not copied.
        assert!(Rc::ptr_eq(&pages[0].dict, &pages[1].dict));
    }

    #[test]
    fn page_tree_billion_laughs_is_a_limit_error() {
        // Ten levels, each listing the next level's node ten times: 10^10
        // pages if shared nodes were expanded.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        for level in 0..10u32 {
            let kids = format!("{} 0 R ", level + 3).repeat(10);
            b.obj(2 + level, &format!("<< /Type /Pages /Kids [{kids}] /Count 1 >>"));
        }
        b.obj(12, "<< /Type /Page /MediaBox [0 0 1 1] >>");
        let doc = open(b.finish_classic(13, "/Root 1 0 R"));
        // With small limits, so that the test is quick in an unoptimised build.
        let small = TreeLimits { pages: 5000, visits: 20_000, repeated_pages: 1 << 20, repeated_bytes: 1 << 30 };
        assert!(matches!(doc.pages_within(small), Err(Error::Limit(m)) if m.contains("5000 pages")));
        let started = std::time::Instant::now();
        assert!(matches!(doc.pages(), Err(Error::Limit(_))));
        assert!(matches!(doc.page_count(), Err(Error::Limit(_))));
        assert!(started.elapsed().as_secs() < 120, "took {:?}", started.elapsed());
    }

    #[test]
    fn a_tree_of_nothing_listed_over_and_over_is_a_limit_error_too() {
        // The same, but the leaves are not pages, so no page is ever counted:
        // the number of entries looked at is what is limited.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        for level in 0..10u32 {
            let kids = format!("{} 0 R ", level + 3).repeat(10);
            b.obj(2 + level, &format!("<< /Type /Pages /Kids [{kids}] /Count 1 >>"));
        }
        b.obj(12, "<< /Type /Font >>");
        let doc = open(b.finish_classic(13, "/Root 1 0 R"));
        let small = TreeLimits { pages: 5000, visits: 20_000, repeated_pages: 1 << 20, repeated_bytes: 1 << 30 };
        assert!(matches!(doc.pages_within(small), Err(Error::Limit(m)) if m.contains("20000 entries")));
    }

    #[test]
    fn one_big_page_listed_over_and_over_is_a_limit_error() {
        // A page of a few KB listed 100 times adds up past a small allowance
        // of bytes; the same page listed 100 times is nothing for the real one.
        let names: String = (0..200).map(|i| format!("/K{i} 1 ")).collect();
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids = "3 0 R ".repeat(100);
        b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 100 >>"));
        b.obj(3, &format!("<< /Type /Page /MediaBox [0 0 1 1] {names} >>"));
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        let tight = TreeLimits { pages: 1000, visits: 1000, repeated_pages: 1000, repeated_bytes: 50_000 };
        assert!(matches!(doc.pages_within(tight), Err(Error::Limit(m)) if m.contains("over and over")));
        // How often is limited too.
        let few = TreeLimits { pages: 1000, visits: 1000, repeated_pages: 10, repeated_bytes: 1 << 30 };
        assert!(matches!(doc.pages_within(few), Err(Error::Limit(m)) if m.contains("listed again more than 10")));
        assert_eq!(doc.pages().unwrap().len(), 100);
    }

    #[test]
    fn names_count_by_their_length_in_what_is_repeated() {
        // The weight of a page is its bytes: one name of 100,000 characters
        // is not one unit. 20 listings of it are over a 1 MiB allowance.
        let big = "A".repeat(100_000);
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids = "3 0 R ".repeat(20);
        b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 20 >>"));
        b.obj(3, &format!("<< /Type /Page /MediaBox [0 0 1 1] /X /{big} >>"));
        let doc = open(b.finish_classic(4, "/Root 1 0 R"));
        let small = TreeLimits { pages: 1000, visits: 1000, repeated_pages: 1000, repeated_bytes: 1 << 20 };
        assert!(matches!(doc.pages_within(small), Err(Error::Limit(_))));
    }

    #[test]
    fn the_object_stream_cache_drops_the_one_used_longest_ago() {
        let stream = |n: u32, size: usize| -> Result<Rc<ObjStm>> {
            let header = format!("{n} 0 ");
            let mut data = format!("{header}(x)").into_bytes();
            data.resize(size, b' ');
            let mut d = Dict::new();
            d.set("N", Object::Integer(1));
            d.set("First", Object::Integer(i64::try_from(header.len()).unwrap()));
            Ok(Rc::new(ObjStm::parse(&d, data).unwrap()))
        };
        let one = stream(1, 4000).unwrap().weight();
        let mut cache = ObjStmCache::with_capacity(one * 2 + one / 2);
        cache.insert(1, &stream(1, 4000));
        cache.insert(2, &stream(2, 4000));
        // 1 is used again, so 2 is the one that goes when 3 needs room.
        assert!(cache.get(1).is_some());
        cache.insert(3, &stream(3, 4000));
        assert!(cache.get(2).is_none(), "the least recently used one is gone");
        assert!(cache.get(1).is_some() && cache.get(3).is_some());
        assert_eq!(cache.len(), 2);
        // A failure is kept too.
        cache.insert(9, &Err(Error::Limit("x".to_string())));
        assert!(matches!(cache.get(9), Some(Err(Error::Limit(_)))));
    }

    #[test]
    fn a_big_resources_dictionary_on_the_root_is_shared_by_every_page() {
        // 300 pages inherit a /Resources of about a megabyte: it is held once.
        let names: String = (0..1000).map(|i| format!("/N{i} /{} ", "A".repeat(1000))).collect();
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..300).map(|i| format!("{} 0 R ", 3 + i)).collect();
        b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 300 /MediaBox [0 0 9 9] /Resources << /X << {names} >> >> >>"));
        for i in 0..300 {
            b.obj(3 + i, "<< /Type /Page /Parent 2 0 R >>");
        }
        let doc = open(b.finish_classic(303, "/Root 1 0 R"));
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 300);
        let id = pages[0].inherited("Resources").unwrap().id;
        assert!(pages.iter().all(|p| p.inherited("Resources").unwrap().id == id));
        assert!(pages.iter().all(|p| p.resources().is_some()));
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
        // Page 8 is listed directly and under node 6: both count.
        assert_eq!(refs, vec![4, 8, 8]);
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
    fn many_object_streams_are_decoded_within_the_cache_and_the_budget() {
        // Each object stream is a megabyte of padding after one small object.
        let mut b = PdfBuilder::with_header("%PDF-1.5\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let n_streams = 4u32;
        let mut offsets = Vec::new();
        for k in 0..n_streams {
            let header = format!("{} 0 ", 10 + k);
            let mut body = format!("{header}(object {k})").into_bytes();
            body.resize(1 << 20, b' ');
            let at = b.flate_stream_obj(100 + k, &format!("/Type /ObjStm /N 1 /First {}", header.len()), &body);
            offsets.push(at);
        }
        let x = b.len();
        let mut rows: Vec<[u8; 4]> = Vec::new();
        let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
        for num in 0..=(100 + n_streams) {
            rows.push(match num {
                1 | 2 => row(1, b.offset_of(num), 0),
                10..=13 => row(2, 100 + (num as usize - 10), 0),
                100..=103 => row(1, offsets[(num - 100) as usize], 0),
                _ => row(0, 0, 0),
            });
        }
        rows.push(row(1, x, 0));
        let packed = png_up_flate(&rows);
        let size = rows.len();
        b.stream_obj(
            104 + 1,
            &format!("/Type /XRef /Size {size} /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>"),
            &packed,
        );
        b.startxref(x);
        let doc = open(b.finish());
        for k in 0..n_streams {
            assert!(matches!(doc.get(ObjRef::new(10 + k, 0)), Ok(Object::String(_))), "{k}");
        }
        // They fit in the cache together, so all four are still there.
        assert_eq!(doc.objstms.borrow().len(), 4);
        assert!(doc.budget.remaining() < filter::DECODE_BUDGET);
    }

    #[test]
    fn decoding_the_same_object_stream_again_and_again_runs_into_the_documents_budget() {
        // An object stream of 1 MiB decoded, and a budget that has room for five
        // such decodes: the later gets are refused. (The cache is emptied each time,
        // so that every get has to decode the stream again.)
        let mut b = PdfBuilder::with_header("%PDF-1.5\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let header = "10 0 ";
        let mut body = format!("{header}(x)").into_bytes();
        body.resize(1 << 20, b' ');
        let at = b.flate_stream_obj(100, &format!("/Type /ObjStm /N 1 /First {}", header.len()), &body);
        let x = b.len();
        let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
        let mut rows: Vec<[u8; 4]> = vec![row(0, 0, 0); 102];
        rows[0] = row(0, 0, 255);
        rows[1] = row(1, b.offset_of(1), 0);
        rows[2] = row(1, b.offset_of(2), 0);
        rows[10] = row(2, 100, 0);
        rows[100] = row(1, at, 0);
        rows[101] = row(1, x, 0);
        let packed = png_up_flate(&rows);
        b.stream_obj(
            101,
            "/Type /XRef /Size 102 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>",
            &packed,
        );
        b.startxref(x);
        let doc = open(b.finish());
        doc.budget.charge_for_test(doc.budget.remaining() - 5 * (1 << 20) - 100);
        let (mut refused, mut read) = (0, 0);
        for _ in 0..8 {
            doc.objstms.borrow_mut().clear();
            match doc.get(ObjRef::new(10, 0)) {
                Ok(Object::String(_)) => read += 1,
                Err(Error::Limit(_)) => refused += 1,
                other => panic!("{other:?}"),
            }
        }
        assert!(read >= 3 && refused >= 2, "{read} read, {refused} refused");
    }

    #[test]
    fn the_object_cache_keeps_what_fits_and_lets_go_on_request() {
        let doc = open(sample_pdf());
        // Off by default.
        doc.get(ObjRef::new(3, 0)).unwrap();
        assert!(doc.cache.borrow().map.is_empty());
        doc.cache_objects(100_000);
        let first = doc.get(ObjRef::new(3, 0)).unwrap();
        assert!(doc.cache.borrow().map.contains_key(&3));
        assert_eq!(doc.get(ObjRef::new(3, 0)).unwrap(), first);
        // A stream is cached with its data.
        let stream = doc.get(ObjRef::new(4, 0)).unwrap();
        assert_eq!(doc.get(ObjRef::new(4, 0)).unwrap(), stream);
        assert!(doc.cache.borrow().bytes > 0);
        // Stopping lets go of everything.
        doc.cache_objects(0);
        assert!(doc.cache.borrow().map.is_empty());
        doc.get(ObjRef::new(3, 0)).unwrap();
        assert!(doc.cache.borrow().map.is_empty());
        // A budget too small for an object keeps nothing of it.
        doc.cache_objects(10);
        assert_eq!(doc.get(ObjRef::new(3, 0)).unwrap(), first);
        assert!(doc.cache.borrow().map.is_empty());
    }

    #[test]
    fn size_counts_nested_content_and_the_length_of_names_and_strings() {
        let mut d = Dict::new();
        d.set("A", Object::Array(vec![Object::Integer(1); 10]));
        d.set("B", Object::Dict(Dict::new()));
        let small = Object::Dict(d).approx_size();
        assert!(small > 200 && small < 400, "{small}");
        let long = Object::Name(crate::object::Name::new(vec![b'x'; 100_000]));
        assert!(long.approx_size() >= 100_000);
        let string = Object::String(crate::object::PdfString::literal(vec![0u8; 5000]));
        assert!(string.approx_size() >= 5000);
    }
}
