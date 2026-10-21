//! Compressing a file (`qingpdf compress`, File > Compress): the same document, written again as a smaller file.
//!
//! What every preset does, and loses nothing by: the objects nobody names are left out (the file is written again from its catalog,
//! like every rewrite); objects that are the same as others are written once ([`survey`]: equal streams with equal dictionaries, equal
//! fonts, colour spaces, resources; a hash of each, and the ones that agree are compared in full before they are merged); the objects that
//! may be are packed into object streams and the table of objects is a compressed cross-reference stream (PDF 1.5); Flate data is deflated
//! again at the best level and data that was not coded is Flate-coded ([`crate::squeeze::tidy_stream`]); a page's thumbnail, private
//! data some programs keep and the metadata of pages and forms are taken out.
//!
//! The lossy presets also make the pictures smaller: how big each is drawn on any page is found by reading the content streams, forms,
//! patterns and appearances ([`Walk`]), and a picture drawn at more dots per inch than the preset keeps is averaged down and coded
//! again as a JPEG ([`crate::squeeze::image`]) on worker threads, where that is smaller. A picture that cannot be changed exactly
//! (a stencil, a bilevel, indexed or CMYK image, one with a `/Decode`, a colour key mask, 16 bits, or one that a pattern or a Type 3 glyph
//! draws) is left as it is.
//!
//! The output is never bigger than the input: [`Packed::data`] is `None` when it would not be smaller. A file that is encrypted stays
//! encrypted the way it was (same method and passwords: the encryption of the input is kept, as for every rewrite), and one that is
//! signed is refused.

use std::collections::{HashMap, HashSet};
use std::hash::Hasher;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::idmap::{IdMap, IdSet};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::ops::{self, Encrypt, Tune};
use crate::render::work::{Work, cost};
use crate::squeeze::image::{self, Codec, Done, Job};
use crate::squeeze::tidy_stream;
use crate::text::scan::{Item, Operand, Scanner};
use crate::writer::Warning;

/// The work compressing one file may do, in the units of the work meter (about a nanosecond each): five minutes of one core.
const ALLOWANCE: f64 = 3.0e11;
/// Forms nest at most this deep for the walk through the content streams, and it reads at most this many operators.
const MAX_FORM_DEPTH: usize = 12;
const MAX_WALK_OPS: usize = 6_000_000;
/// Decoded contents of forms kept for the walk, in bytes.
const FORM_CACHE_BYTES: usize = 24 << 20;
/// A picture drawn at up to this many times the dots per inch of the preset is left at its size (it is still coded again if that is smaller).
const SLACK: f64 = 1.1;
/// Pictures of fewer pixels or fewer bytes than this are left alone.
const MIN_PIXELS: usize = 2_500;
const MIN_BYTES: usize = 2_048;

/// How much the pictures are given up: dots per inch they are kept at, and the JPEG quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// 200 dpi, quality 85.
    High,
    /// 150 dpi, quality 75 (the one to take).
    Balanced,
    /// 100 dpi, quality 60.
    Small,
}

impl Preset {
    pub fn dpi(self) -> u32 {
        match self {
            Preset::High => 200,
            Preset::Balanced => 150,
            Preset::Small => 100,
        }
    }

    pub fn quality(self) -> u8 {
        match self {
            Preset::High => 85,
            Preset::Balanced => 75,
            Preset::Small => 60,
        }
    }
}

/// What to do.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub preset: Preset,
    /// Only what loses nothing: the pictures are not changed.
    pub lossless_only: bool,
    /// Bytes the worker threads may hold at once for the pictures they work on (a picture bigger than this works alone).
    pub picture_memory: u64,
    /// Worker threads (0: as many as the machine has, up to 8).
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options { preset: Preset::Balanced, lossless_only: false, picture_memory: 256 << 20, threads: 0 }
    }
}

/// How the caller follows and stops the work.
pub struct Control<'a> {
    /// Set to stop: the work ends with [`Error::Cancelled`] at its next step.
    pub stop: Option<Arc<AtomicBool>>,
    /// Told how far it is, 0 to 100, from time to time (on the thread that called).
    pub progress: &'a dyn Fn(u32),
}

/// What came of it.
#[derive(Debug)]
pub struct Packed {
    /// The new file; `None` when it would not be smaller than the old one (which is then to be kept).
    pub data: Option<Vec<u8>>,
    /// The size of the old file and of what is to be kept: the new one, or the old one again.
    pub before: u64,
    pub after: u64,
    pub warnings: Vec<Warning>,
    /// How many pictures were made smaller, and how many objects were merged into others.
    pub pictures: u32,
    pub merged: u32,
}

fn stopped(work: &Work) -> Error {
    if work.was_cancelled() {
        Error::Cancelled
    } else {
        Error::Limit("compressing this file asks for more work than is allowed".to_string())
    }
}

fn spend(work: &Work, units: f64) -> Result<()> {
    if work.charge(units) { Ok(()) } else { Err(stopped(work)) }
}

// --- which objects are the same ---------------------------------------------------------------------------------------

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` then `b` (8.3.4).
fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn matrix_of(o: Option<&Object>) -> Matrix {
    let Some([a, b, c, d, e, f]) = o.and_then(Object::as_array) else { return IDENTITY };
    let n = |o: &Object| o.as_f64().filter(|v| v.is_finite());
    match (n(a), n(b), n(c), n(d), n(e), n(f)) {
        (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) => [a, b, c, d, e, f],
        _ => IDENTITY,
    }
}

/// A matrix entry as a whole number that stays the same for a change of a few per cent and differs for a bigger one, however big or small
/// the entry is (a form drawn at 0.01 and again at 0.05 is drawn at two sizes), and for its sign.
fn relative(v: f64) -> i64 {
    if !v.is_finite() || v.abs() < 1e-12 {
        return 0;
    }
    (v.signum() as i64) * ((v.abs().ln() * 32.0).round() as i64 + 100_000)
}

/// A form XObject, as far as the walk needs it.
struct Form {
    matrix: Matrix,
    bbox: Option<[f64; 4]>,
    resources: Option<Object>,
}

/// What looking at every object of the file found.
struct Survey {
    /// Object number -> the number of the object it is the same as (the first of them met).
    merged: IdMap<u32, u32>,
    /// The picture streams (numbers of the objects that stay), the forms, a picture's soft mask, and how many pictures name a mask.
    images: Vec<u32>,
    image_set: IdSet<u32>,
    forms: IdMap<u32, Form>,
    masks: IdMap<u32, u32>,
    mask_users: IdMap<u32, u32>,
}

impl Survey {
    fn canon(&self, num: u32) -> u32 {
        self.merged.get(&num).copied().unwrap_or(num)
    }
}

/// A set of object numbers, a bit each.
#[derive(Default)]
struct Bits(Vec<u64>);

impl Bits {
    fn has(&self, n: u32) -> bool {
        self.0.get((n / 64) as usize).is_some_and(|w| w >> (n % 64) & 1 == 1)
    }

    fn set(&mut self, n: u32, on: bool) {
        let at = (n / 64) as usize;
        if self.0.len() <= at {
            self.0.resize(at + 1, 0);
        }
        if let Some(w) = self.0.get_mut(at) {
            if on {
                *w |= 1 << (n % 64);
            } else {
                *w &= !(1 << (n % 64));
            }
        }
    }
}

/// 128 bits from the bytes: two runs of the standard library's hash with different starts.
fn digest(bytes: &[u8]) -> u128 {
    let mut a = std::hash::DefaultHasher::new();
    let mut b = std::hash::DefaultHasher::new();
    a.write(b"qingpdf-same-1");
    b.write(b"qingpdf-same-2");
    a.write(bytes);
    b.write(bytes);
    a.write_usize(bytes.len());
    b.write_usize(bytes.len());
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}

/// Every reference in `obj`, in order.
fn refs_in(obj: &Object, out: &mut Vec<ObjRef>) {
    match obj {
        Object::Ref(r) => out.push(*r),
        Object::Array(items) => items.iter().for_each(|o| refs_in(o, out)),
        Object::Dict(d) => d.iter().for_each(|(_, o)| refs_in(o, out)),
        Object::Stream(s) => s.dict.iter().for_each(|(_, o)| refs_in(o, out)),
        _ => {}
    }
}

/// The object as bytes that are the same for two objects that are the same, whatever numbers their references have: a reference is
/// written as the number of the object it is merged into. A stream's `/Length` is left out (the file gets its own), and the data stays
/// out too (`data` is its hash and length instead).
fn canon(obj: &Object, class: &dyn Fn(u32) -> u32, data: Option<(u128, usize)>, out: &mut Vec<u8>) {
    match obj {
        Object::Null => out.push(0),
        Object::Bool(b) => out.extend_from_slice(&[1, u8::from(*b)]),
        Object::Integer(i) => {
            out.push(2);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Object::Real(r) => {
            out.push(3);
            out.extend_from_slice(&r.to_bits().to_le_bytes());
        }
        Object::String(s) => {
            out.push(4);
            out.extend_from_slice(&(s.bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(&s.bytes);
        }
        Object::Name(n) => {
            out.push(5);
            out.extend_from_slice(&(n.as_bytes().len() as u64).to_le_bytes());
            out.extend_from_slice(n.as_bytes());
        }
        Object::Array(items) => {
            out.push(6);
            out.extend_from_slice(&(items.len() as u64).to_le_bytes());
            items.iter().for_each(|o| canon(o, class, None, out));
        }
        Object::Dict(d) => canon_dict(d, class, out),
        Object::Stream(s) => {
            out.push(7);
            canon_dict(&s.dict, class, out);
            if let Some((hash, len)) = data {
                out.extend_from_slice(&hash.to_le_bytes());
                out.extend_from_slice(&(len as u64).to_le_bytes());
            }
        }
        Object::Ref(r) => {
            out.push(9);
            out.extend_from_slice(&class(r.num).to_le_bytes());
        }
    }
}

fn canon_dict(d: &Dict, class: &dyn Fn(u32) -> u32, out: &mut Vec<u8>) {
    out.push(8);
    for (k, v) in d.iter().filter(|(k, _)| *k != "Length") {
        out.extend_from_slice(&(k.as_bytes().len() as u64).to_le_bytes());
        out.extend_from_slice(k.as_bytes());
        canon(v, class, None, out);
    }
    out.push(0xFF);
}

/// May an object that is the same as another be merged with it? Not one that something names by what it is rather than by what it
/// says: pages, annotations, form fields, outline items, layers (two layers with the same name are two layers), the nodes of trees.
fn mergeable(obj: &Object) -> bool {
    /// The keys that tie a dictionary to one place.
    const PLACED: [&str; 18] = ["Parent", "P", "Kids", "Rect", "FT", "T", "A", "AA", "Dest", "Prev", "Next", "First", "Last", "Pg", "K", "Names", "Limits", "V"];
    /// The kinds of dictionary that are merged: all of them describe what something is and not where it is.
    const KINDS: [&str; 7] = ["Font", "FontDescriptor", "ExtGState", "Encoding", "Pattern", "Group", "Mask"];
    match obj {
        Object::Null | Object::Ref(_) => false,
        Object::Stream(s) => !["Metadata", "XRef", "ObjStm"].iter().any(|t| s.dict.get_name("Type").is_some_and(|n| n == t)),
        Object::Dict(d) => match d.get_name("Type") {
            Some(t) => KINDS.iter().any(|k| t == k),
            None => !PLACED.iter().any(|k| d.contains_key(k)),
        },
        _ => true,
    }
}

/// An object being looked at: kept while the objects it names are looked at.
struct Frame {
    r: ObjRef,
    /// The object, a stream's data taken out.
    obj: Object,
    /// A stream's data: its hash and length.
    data: Option<(u128, usize)>,
    kids: Vec<ObjRef>,
    at: usize,
    /// Something it names leads back to it (a page and its annotations): it is not merged.
    cyclic: bool,
}

fn open_frame(doc: &Document, work: &Work, r: ObjRef) -> Result<Frame> {
    let obj = match doc.get(r) {
        Ok(o) => o,
        Err(e @ (Error::Limit(_) | Error::Cancelled | Error::Io(_) | Error::Unsupported(_))) => return Err(e),
        Err(_) => Object::Null,
    };
    let (obj, data) = match obj {
        Object::Stream(Stream { dict, data }) => {
            spend(work, data.len() as f64 * cost::SQZ_HASH_BYTE)?;
            let key = Some((digest(&data), data.len()));
            (Object::Stream(Stream { dict, data: Vec::new() }), key)
        }
        other => (other, None),
    };
    spend(work, cost::SQZ_OBJECT)?;
    let mut kids = Vec::new();
    refs_in(&obj, &mut kids);
    Ok(Frame { r, obj, data, kids, at: 0, cyclic: false })
}

/// Are the object `rep` and the one in `frame` really the same (their hashes agreed)? Looked at again in full.
fn really_same(doc: &Document, rep: u32, frame: &Frame, class: &dyn Fn(u32) -> u32) -> bool {
    let (Ok(x), Ok(y)) = (doc.get(ObjRef::new(rep, 0)), doc.get(frame.r)) else { return false };
    let (mut p, mut q) = (Vec::new(), Vec::new());
    canon(&x, class, None, &mut p);
    canon(&y, class, None, &mut q);
    p == q
        && match (&x, &y) {
            (Object::Stream(s), Object::Stream(t)) => s.data == t.data,
            _ => true,
        }
}

/// Look at every object that can be reached from the catalog and the information dictionary, children before the objects that name
/// them, and find the ones that are the same as others.
fn survey(doc: &Document, work: &Work, ctl: &Control<'_>) -> Result<Survey> {
    let mut out = Survey { merged: IdMap::new(), images: Vec::new(), image_set: IdSet::new(), forms: IdMap::new(), masks: IdMap::new(), mask_users: IdMap::new() };
    let (mut done, mut open) = (Bits::default(), Bits::default());
    let mut table: HashMap<u128, u32> = HashMap::new();
    // Objects that something reached through them leads back to.
    let mut cyclic: IdSet<u32> = IdSet::new();
    let total = f64::from(doc.next_free_number().max(1));
    let mut looked = 0u32;
    let roots: Vec<ObjRef> = ["Root", "Info"].iter().filter_map(|k| doc.trailer().get(k).and_then(Object::as_obj_ref)).collect();
    for root in roots {
        if done.has(root.num) {
            continue;
        }
        let mut stack = vec![open_frame(doc, work, root)?];
        open.set(root.num, true);
        while let Some(top) = stack.last_mut() {
            if let Some(kid) = top.kids.get(top.at).copied() {
                top.at += 1;
                if done.has(kid.num) {
                    continue;
                }
                if open.has(kid.num) {
                    top.cyclic = true;
                    cyclic.insert(kid.num);
                    continue;
                }
                if stack.len() > 100_000 {
                    return Err(Error::Limit("objects lead one to the next more than 100,000 deep".to_string()));
                }
                open.set(kid.num, true);
                stack.push(open_frame(doc, work, kid)?);
                continue;
            }
            let Some(frame) = stack.pop() else { break };
            open.set(frame.r.num, false);
            done.set(frame.r.num, true);
            looked += 1;
            if looked.is_multiple_of(512) {
                (ctl.progress)((f64::from(looked) / total * 22.0).min(22.0) as u32);
            }
            let class = |n: u32| out.merged.get(&n).copied().unwrap_or(n);
            let mut rep = frame.r.num;
            if !frame.cyclic && !cyclic.contains(&frame.r.num) && mergeable(&frame.obj) {
                let mut bytes = Vec::new();
                canon(&frame.obj, &class, frame.data, &mut bytes);
                let key = digest(&bytes);
                match table.get(&key).copied() {
                    Some(other) if really_same(doc, other, &frame, &class) => rep = other,
                    Some(_) => {}
                    None => {
                        table.insert(key, frame.r.num);
                    }
                }
            }
            if rep != frame.r.num {
                out.merged.insert(frame.r.num, rep);
                continue;
            }
            if let Object::Stream(s) = &frame.obj {
                match s.dict.get_name("Subtype") {
                    Some(n) if n == "Image" => {
                        out.images.push(rep);
                        out.image_set.insert(rep);
                        if let Some(Object::Ref(m)) = s.dict.get("SMask") {
                            let m = class(m.num);
                            out.masks.insert(rep, m);
                            out.mask_users.update(m, 0, |n| *n += 1);
                        }
                    }
                    Some(n) if n == "Form" => {
                        let bbox = s.dict.get("BBox").and_then(Object::as_array).and_then(|a| match a {
                            [x0, y0, x1, y1] => Some([x0.as_f64()?, y0.as_f64()?, x1.as_f64()?, y1.as_f64()?]),
                            _ => None,
                        });
                        out.forms.insert(rep, Form { matrix: matrix_of(s.dict.get("Matrix")), bbox, resources: s.dict.get("Resources").cloned() });
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

// --- how big each picture is drawn -------------------------------------------------------------------------------------

/// How a picture is used: the most points wide and high a page draws it, and whether somewhere that cannot be measured draws it.
#[derive(Default)]
struct Use {
    w: f64,
    h: f64,
    pinned: bool,
}

/// The names of a resource dictionary's `/XObject`, with the numbers (merged ones turned into the one they are merged into).
type Names = Rc<HashMap<Vec<u8>, u32>>;

struct Walk<'a> {
    doc: &'a Document,
    survey: &'a Survey,
    work: &'a Work,
    uses: IdMap<u32, Use>,
    /// The forms walked at a size already, and the ones whose resources were looked at for what draws pictures where nothing can measure.
    seen: HashSet<(u32, [i64; 4])>,
    scanned: IdSet<u32>,
    names: IdMap<u32, Names>,
    contents: IdMap<u32, Rc<Vec<u8>>>,
    content_bytes: usize,
    /// The operators a page may run (forms included); every page starts again, the work meter bounds the whole.
    ops_limit: usize,
    ops_left: usize,
}

impl<'a> Walk<'a> {
    fn new(doc: &'a Document, survey: &'a Survey, work: &'a Work) -> Walk<'a> {
        Walk { doc, survey, work, uses: IdMap::new(), seen: HashSet::new(), scanned: IdSet::new(), names: IdMap::new(), contents: IdMap::new(), content_bytes: 0, ops_limit: MAX_WALK_OPS, ops_left: MAX_WALK_OPS }
    }

    /// The `/XObject` names of `resources`.
    fn names(&mut self, resources: &Object) -> Result<Names> {
        if let Object::Ref(r) = resources
            && let Some(known) = self.names.get(&r.num)
        {
            return Ok(known.clone());
        }
        let mut map = HashMap::new();
        if let Ok(Object::Dict(res)) = self.doc.resolve(resources)
            && let Some(Ok(Object::Dict(xo))) = res.get("XObject").map(|o| self.doc.resolve(o))
        {
            for (k, v) in xo.iter() {
                if let Object::Ref(r) = v {
                    map.insert(k.as_bytes().to_vec(), self.survey.canon(r.num));
                }
            }
        }
        let names = Rc::new(map);
        if let Object::Ref(r) = resources {
            self.names.insert(r.num, names.clone());
        }
        Ok(names)
    }

    fn note(&mut self, picture: u32, ctm: &Matrix, pinned: bool) {
        self.uses.update(picture, Use::default(), |u| {
            if pinned {
                u.pinned = true;
            } else {
                u.w = u.w.max(ctm[0].hypot(ctm[1]));
                u.h = u.h.max(ctm[2].hypot(ctm[3]));
            }
        });
    }

    /// The decoded content of the form `num`, kept for the next time while there is room.
    fn content(&mut self, num: u32) -> Result<Option<Rc<Vec<u8>>>> {
        if let Some(c) = self.contents.get(&num) {
            return Ok(Some(c.clone()));
        }
        let Ok(Object::Stream(s)) = self.doc.get(ObjRef::new(num, 0)) else { return Ok(None) };
        spend(self.work, s.data.len() as f64 * cost::SQZ_WALK_BYTE)?;
        let data = match self.doc.decode_stream(&s) {
            Ok(d) => Rc::new(d),
            Err(e @ (Error::Limit(_) | Error::Cancelled)) => return Err(e),
            Err(_) => return Ok(None),
        };
        if self.content_bytes.saturating_add(data.len()) <= FORM_CACHE_BYTES {
            self.content_bytes += data.len();
            self.contents.insert(num, data.clone());
        }
        Ok(Some(data))
    }

    /// Read a content stream: which pictures are drawn at what size, through the forms it draws (`pinned`: whatever is found here is drawn
    /// where nothing can be measured). `resources` are the ones in force.
    fn run(&mut self, content: &[u8], resources: &Object, ctm: Matrix, depth: usize, pinned: bool) -> Result<()> {
        self.pin_scan(resources, depth)?;
        let names = self.names(resources)?;
        let mut sc = Scanner::new(content);
        let mut ops: Vec<Operand> = Vec::new();
        let (mut ctm, mut stack) = (ctm, Vec::<Matrix>::new());
        loop {
            let Item::Operator(op) = sc.next(&mut ops) else { return Ok(()) };
            if self.ops_left == 0 {
                return Err(Error::Limit("a page has more content stream operators than compressing reads".to_string()));
            }
            self.ops_left -= 1;
            spend(self.work, cost::SQZ_WALK_OP)?;
            match op {
                b"q" => {
                    if stack.len() < 256 {
                        stack.push(ctm);
                    }
                }
                b"Q" => {
                    if let Some(m) = stack.pop() {
                        ctm = m;
                    }
                }
                b"cm" => {
                    let nums: Vec<f64> = ops.iter().filter_map(|o| if let Operand::Num(n) = o { Some(*n) } else { None }).collect();
                    if let [.., a, b, c, d, e, f] = nums.as_slice() {
                        let m = mul(&[*a, *b, *c, *d, *e, *f], &ctm);
                        if m.iter().all(|v| v.is_finite()) {
                            ctm = m;
                        }
                    }
                }
                b"Do" => {
                    if let Some(Operand::Name(span)) = ops.last()
                        && let Some(&num) = names.get(sc.bytes(*span))
                    {
                        if self.survey.image_set.contains(&num) {
                            self.note(num, &ctm, pinned);
                        } else if depth < MAX_FORM_DEPTH {
                            self.form(num, &ctm, resources, depth, pinned)?;
                        }
                    }
                }
                b"BI" => {
                    for _ in 0..4096 {
                        match sc.next(&mut ops) {
                            Item::Operator(b"ID") => {
                                sc.skip_inline_image_data();
                                break;
                            }
                            Item::Operator(_) => {}
                            Item::End => return Ok(()),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Draw the form `num` at `ctm`.
    fn form(&mut self, num: u32, ctm: &Matrix, outer: &Object, depth: usize, pinned: bool) -> Result<()> {
        let survey = self.survey;
        let Some(form) = survey.forms.get(&num) else { return Ok(()) };
        let at = mul(&form.matrix, ctm);
        // Once for each size: a form shown a thousand times at one size is read once.
        let key = (num, [0, 1, 2, 3].map(|i| relative(at.get(i).copied().unwrap_or(0.0))));
        if !pinned && !self.seen.insert(key) {
            return Ok(());
        }
        if pinned && !self.seen.insert((num, [i64::MAX; 4])) {
            return Ok(());
        }
        let Some(content) = self.content(num)? else { return Ok(()) };
        let resources = form.resources.clone().unwrap_or_else(|| outer.clone());
        self.run(&content, &resources, at, depth + 1, pinned)
    }

    /// The resources of a page or form can have patterns, Type 3 glyphs and soft masks: what they draw is drawn in a space no page size
    /// tells about, so the pictures in them are left as they are.
    fn pin_scan(&mut self, resources: &Object, depth: usize) -> Result<()> {
        if let Object::Ref(r) = resources
            && !self.scanned.insert(r.num)
        {
            return Ok(());
        }
        let Ok(Object::Dict(res)) = self.doc.resolve(resources) else { return Ok(()) };
        let sub = |key: &str| match res.get(key).map(|o| self.doc.resolve(o)) {
            Some(Ok(Object::Dict(d))) => Some(d),
            _ => None,
        };
        let (patterns, fonts, states) = (sub("Pattern"), sub("Font"), sub("ExtGState"));
        let parent = resources.clone();
        for (_, v) in patterns.iter().flat_map(|d| d.iter()) {
            if let Object::Ref(r) = v {
                self.pinned_stream(r.num, &parent, depth)?;
            }
        }
        for (_, v) in fonts.iter().flat_map(|d| d.iter()) {
            if let Object::Ref(r) = v
                && !self.scanned.insert(r.num)
            {
                continue;
            }
            let Ok(Object::Dict(font)) = self.doc.resolve(v) else { continue };
            if !font.get_name("Subtype").is_some_and(|n| n == "Type3") {
                continue;
            }
            let own = font.get("Resources").cloned().unwrap_or_else(|| parent.clone());
            if let Some(Ok(Object::Dict(procs))) = font.get("CharProcs").map(|o| self.doc.resolve(o)) {
                for (_, p) in procs.iter() {
                    if let Object::Ref(r) = p {
                        self.pinned_stream(r.num, &own, depth)?;
                    }
                }
            }
        }
        for (_, v) in states.iter().flat_map(|d| d.iter()) {
            if let Object::Ref(r) = v
                && !self.scanned.insert(r.num)
            {
                continue;
            }
            if let Ok(Object::Dict(gs)) = self.doc.resolve(v)
                && let Some(Ok(Object::Dict(mask))) = gs.get("SMask").map(|o| self.doc.resolve(o))
                && let Some(Object::Ref(g)) = mask.get("G")
            {
                self.pinned_stream(g.num, &parent, depth)?;
            }
        }
        Ok(())
    }

    /// A pattern, a glyph's procedure or a mask's group: read for the pictures it draws, all of which are left as they are.
    fn pinned_stream(&mut self, num: u32, outer: &Object, depth: usize) -> Result<()> {
        if depth >= MAX_FORM_DEPTH || !self.seen.insert((num, [i64::MIN; 4])) {
            return Ok(());
        }
        let Ok(Object::Stream(s)) = self.doc.get(ObjRef::new(num, 0)) else { return Ok(()) };
        spend(self.work, s.data.len() as f64 * cost::SQZ_WALK_BYTE)?;
        let content = match self.doc.decode_stream(&s) {
            Ok(c) => c,
            Err(e @ (Error::Limit(_) | Error::Cancelled)) => return Err(e),
            Err(_) => return Ok(()),
        };
        let resources = s.dict.get("Resources").cloned().unwrap_or_else(|| outer.clone());
        self.run(&content, &resources, IDENTITY, depth + 1, true)
    }

    /// The pictures one page draws: its content, and the appearances of its annotations.
    fn page(&mut self, page: &Page) -> Result<()> {
        self.ops_left = self.ops_limit;
        let resources = page.resources().cloned().unwrap_or(Object::Null);
        let unit = page.dict.get("UserUnit").and_then(Object::as_f64).filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1.0);
        let base: Matrix = [unit, 0.0, 0.0, unit, 0.0, 0.0];
        // (What its patterns, Type 3 glyphs and soft masks draw is looked at whether or not the page itself names a picture.)
        self.pin_scan(&resources, 0)?;
        // A page whose resources name no XObject draws no picture: its content is not read.
        if !self.names(&resources)?.is_empty() {
            let mut parts: Vec<Object> = Vec::new();
            match page.dict.get("Contents").map(|o| self.doc.resolve(o)) {
                Some(Ok(s @ Object::Stream(_))) => parts.push(s),
                Some(Ok(Object::Array(items))) => parts.extend(items.iter().filter_map(|o| self.doc.resolve(o).ok())),
                _ => {}
            }
            // The streams of /Contents are one program (7.8.2): joined, since an operator may be cut between them.
            let mut content = Vec::new();
            for part in &parts {
                if let Object::Stream(s) = part {
                    spend(self.work, s.data.len() as f64 * cost::SQZ_WALK_BYTE)?;
                    match self.doc.decode_stream(s) {
                        Ok(d) => content.extend_from_slice(&d),
                        Err(e @ (Error::Limit(_) | Error::Cancelled)) => return Err(e),
                        Err(_) => {}
                    }
                    content.push(b'\n');
                }
            }
            self.run(&content, &resources, base, 0, false)?;
        }
        // Annotations: each appearance is drawn where the annotation's rectangle is (12.5.5, Algorithm 8.1).
        let Some(Ok(Object::Array(annots))) = page.dict.get("Annots").map(|o| self.doc.resolve(o)) else { return Ok(()) };
        for a in annots.iter().take(20_000) {
            let Ok(Object::Dict(annot)) = self.doc.resolve(a) else { continue };
            let Some(rect) = annot.get("Rect").and_then(Object::as_array).and_then(|r| match r {
                [a, b, c, d] => Some([a.as_f64()?, b.as_f64()?, c.as_f64()?, d.as_f64()?]),
                _ => None,
            }) else {
                continue;
            };
            let Some(Ok(Object::Dict(ap))) = annot.get("AP").map(|o| self.doc.resolve(o)) else { continue };
            let mut streams: Vec<u32> = Vec::new();
            match ap.get("N") {
                Some(Object::Ref(r)) if self.survey.forms.contains_key(&self.survey.canon(r.num)) => streams.push(self.survey.canon(r.num)),
                Some(n) => {
                    if let Ok(Object::Dict(states)) = self.doc.resolve(n) {
                        streams.extend(states.iter().filter_map(|(_, v)| v.as_obj_ref()).map(|r| self.survey.canon(r.num)));
                    }
                }
                None => {}
            }
            for num in streams {
                let Some(form) = self.survey.forms.get(&num) else { continue };
                let Some(bbox) = form.bbox else { continue };
                // The box of the form after its matrix, fitted to the annotation's rectangle.
                let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[0], bbox[3]), (bbox[2], bbox[3])].map(|(x, y)| {
                    let m = &form.matrix;
                    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
                });
                let lo = corners.iter().fold((f64::MAX, f64::MAX), |a, c| (a.0.min(c.0), a.1.min(c.1)));
                let hi = corners.iter().fold((f64::MIN, f64::MIN), |a, c| (a.0.max(c.0), a.1.max(c.1)));
                let (rx0, ry0, rx1, ry1) = (rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3]));
                let sx = if hi.0 - lo.0 > 1e-9 { (rx1 - rx0) / (hi.0 - lo.0) } else { 1.0 };
                let sy = if hi.1 - lo.1 > 1e-9 { (ry1 - ry0) / (hi.1 - lo.1) } else { 1.0 };
                let fit: Matrix = [sx, 0.0, 0.0, sy, rx0 - lo.0 * sx, ry0 - lo.1 * sy];
                if fit.iter().all(|v| v.is_finite()) {
                    self.form(num, &mul(&fit, &base), &resources, 0, false)?;
                }
            }
        }
        Ok(())
    }
}

// --- the pictures --------------------------------------------------------------------------------------------------------

/// The number of colour components of a picture's `/ColorSpace` when it is one that keeps its meaning as the samples of a JPEG
/// (grey or RGB, calibrated or from a profile): 1 or 3. `None` for the rest.
fn components(doc: &Document, space: &Object) -> Option<usize> {
    let space = doc.resolve(space).ok()?;
    match &space {
        Object::Name(n) if n == "DeviceGray" => Some(1),
        Object::Name(n) if n == "DeviceRGB" => Some(3),
        Object::Array(items) => match items.first().and_then(Object::as_name) {
            Some(n) if n == "CalGray" => Some(1),
            Some(n) if n == "CalRGB" => Some(3),
            Some(n) if n == "ICCBased" => {
                let Ok(Object::Stream(profile)) = doc.resolve(items.get(1)?) else { return None };
                match profile.dict.get_int("N") {
                    Some(1) => Some(1),
                    Some(3) => Some(3),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// The names in a `/Filter` entry (a name or an array of names, written in place).
fn filter_names(dict: &Dict) -> Option<Vec<Vec<u8>>> {
    match dict.get("Filter") {
        None => Some(Vec::new()),
        Some(Object::Name(n)) => Some(vec![n.as_bytes().to_vec()]),
        Some(Object::Array(items)) => items.iter().map(|o| o.as_name().map(|n| n.as_bytes().to_vec())).collect(),
        _ => None,
    }
}

/// The dictionary with its `/Filter` and `/DecodeParms` written in place (a worker has no document to follow a reference in).
fn plain_filters(doc: &Document, dict: &Dict) -> Dict {
    let mut d = dict.clone();
    for key in ["Filter", "DecodeParms"] {
        if let Some(v) = dict.get(key) {
            let resolved = match doc.resolve(v) {
                Ok(Object::Array(items)) => Object::Array(items.iter().map(|o| doc.resolve(o).unwrap_or(Object::Null)).collect()),
                Ok(other) => other,
                Err(_) => Object::Null,
            };
            d.set(key, resolved);
        }
    }
    d
}

/// Can the stream of the soft mask `num` be made smaller with its picture: eight-bit grey, no `/Decode`, no matte, Flate-coded or not coded?
fn mask_job(doc: &Document, num: u32) -> Option<Stream> {
    let Ok(Object::Stream(s)) = doc.get(ObjRef::new(num, 0)) else { return None };
    let d = &s.dict;
    let grey = matches!(d.get("ColorSpace"), Some(Object::Name(n)) if n == "DeviceGray");
    let names = filter_names(d)?;
    let flate_or_none = names.iter().all(|n| n == b"FlateDecode");
    let (w, h) = (d.get_int("Width")?, d.get_int("Height")?);
    (grey && flate_or_none && d.get_int("BitsPerComponent") == Some(8) && !d.contains_key("Decode") && !d.contains_key("Matte") && w > 0 && h > 0)
        .then(|| Stream { dict: plain_filters(doc, d), data: s.data })
}

/// Does a JPEG's `/DecodeParms` (a dictionary, or one for each filter) have a `/ColorTransform` other than 1 (YCbCr, what three components
/// are taken to be)?
fn colour_transform_is_off(dict: &Dict) -> bool {
    let off = |p: &Dict| p.contains_key("ColorTransform") && p.get_int("ColorTransform") != Some(1);
    match dict.get("DecodeParms") {
        Some(Object::Dict(p)) => off(p),
        Some(Object::Array(items)) => items.iter().any(|o| matches!(o, Object::Dict(p) if off(p))),
        _ => false,
    }
}

/// The job for the picture `num`, if it can be made smaller exactly as far as the preset says: what it is, what it is drawn at.
fn picture_job(doc: &Document, survey: &Survey, uses: &IdMap<u32, Use>, num: u32, how: &Options) -> Option<Job> {
    let u = uses.get(&num).filter(|u| !u.pinned && u.w > 0.0 && u.h > 0.0)?;
    // (A picture that is the soft mask of another is changed with that one, or not at all.)
    if survey.mask_users.contains_key(&num) {
        return None;
    }
    let Ok(Object::Stream(s)) = doc.get(ObjRef::new(num, 0)) else { return None };
    let d = &s.dict;
    let to_usize = |key: &str| d.get_int(key).and_then(|v| usize::try_from(v).ok()).filter(|&v| v > 0);
    let (w, h) = (to_usize("Width")?, to_usize("Height")?);
    if w.saturating_mul(h) < MIN_PIXELS || s.data.len() < MIN_BYTES || w.saturating_mul(h) > image::MAX_SOURCE_PIXELS {
        return None;
    }
    // What cannot be changed exactly: stencils, colour key masks and explicit masks, a /Decode, an opacity channel in the file.
    if matches!(d.get("ImageMask"), Some(Object::Bool(true))) || ["Mask", "Decode", "Matte"].iter().any(|k| d.contains_key(k)) || d.get_int("SMaskInData").is_some_and(|v| v != 0) {
        return None;
    }
    let names = filter_names(d)?;
    let plain = plain_filters(doc, d);
    let codec = match names.as_slice() {
        [one] if one == b"DCTDecode" => Codec::Jpeg,
        [one] if one == b"JPXDecode" => Codec::Jpx,
        list if list.iter().all(|n| matches!(n.as_slice(), b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode" | b"RunLengthDecode")) => Codec::Raw,
        _ => return None,
    };
    let comps = match d.get("ColorSpace") {
        Some(space) => components(doc, space)?,
        None if codec == Codec::Jpx => 0,
        None => return None,
    };
    if codec == Codec::Raw && d.get_int("BitsPerComponent") != Some(8) {
        return None;
    }
    // (/ColorTransform in a JPEG's /DecodeParms says whether three components are YCbCr; the decoder here goes by the file's own marks, so a
    // picture that says they are not is left as it is.)
    if codec == Codec::Jpeg && comps == 3 && colour_transform_is_off(&plain) {
        return None;
    }
    // The size it is drawn at, at the most dots per inch the preset keeps (a picture within a tenth of that stays as it is).
    let dpi = f64::from(how.preset.dpi());
    let wanted = |pixels: usize, points: f64| {
        let need = points * dpi / 72.0;
        if pixels as f64 > need * SLACK { (need.ceil().max(1.0) as usize).min(pixels) } else { pixels }
    };
    let target = (wanted(w, u.w), wanted(h, u.h));
    // A JPEG 2000 picture is decoded slowly and rarely beaten by a JPEG of its size: only one that is made smaller is looked at.
    if codec == Codec::Jpx && target == (w, h) {
        return None;
    }
    let mask = survey
        .masks
        .get(&num)
        .copied()
        .filter(|m| survey.mask_users.get(m) == Some(&1) && survey.image_set.contains(m) && !uses.contains_key(m))
        .and_then(|m| mask_job(doc, m).map(|s| (m, s)));
    // A mask that is blended with a matte colour must be the picture's size (11.6.5.3): the picture stays as it is then.
    if mask.is_none() && survey.masks.get(&num).is_some_and(|&m| matches!(doc.get(ObjRef::new(m, 0)), Ok(Object::Stream(s)) if s.dict.contains_key("Matte"))) {
        return None;
    }
    let mut job = Job { stream: Stream { dict: plain, data: s.data }, codec, size: (w, h), comps, target, quality: how.preset.quality(), mask };
    // A mask of a size that does not fit the memory with the picture stays as it is; the picture is still made smaller.
    if job.mask.is_some() && image::memory(&job) > how.picture_memory {
        job.mask = None;
    }
    Some(job)
}

/// How many threads make pictures smaller: those asked for, or the machine's, at most 8.
fn thread_count(how: &Options) -> usize {
    if how.threads > 0 { how.threads } else { std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) }
}

/// Make the pictures of `jobs` smaller, on this thread and up to `threads` - 1 more that it starts for the time it takes (a thread that
/// cannot be started leaves its share to the others).
fn run_batch(jobs: Vec<(u32, Job)>, threads: usize, work: &Work) -> Vec<(u32, Result<Done>)> {
    let more = threads.min(jobs.len()).saturating_sub(1);
    let queue = Mutex::new(jobs);
    let results = Mutex::new(Vec::new());
    let worker = || {
        loop {
            // (The lock is let go of before the work: a guard in the head of a `while let` is held through its body.)
            let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop();
            let Some((num, job)) = next else { break };
            let result = image::shrink(job, work);
            results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((num, result));
        }
    };
    std::thread::scope(|scope| {
        for _ in 0..more {
            let _ = std::thread::Builder::new().spawn_scoped(scope, worker);
        }
        worker();
    });
    results.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Make the pictures smaller, a batch at a time: as many as the threads can hold in the memory they are lent (a picture that wants more
/// than that is a batch of its own). The new streams, by the number of the object they replace.
fn shrink_pictures(
    doc: &Document,
    survey: &Survey,
    uses: &IdMap<u32, Use>,
    how: &Options,
    work: &Work,
    ctl: &Control<'_>,
    warnings: &mut Vec<Warning>,
) -> Result<(IdMap<u32, Object>, u32)> {
    let threads = thread_count(how);
    let total = survey.images.len().max(1);
    let (mut batch, mut held) = (Vec::new(), 0u64);
    let mut results = Vec::new();
    for (looked, &num) in survey.images.iter().enumerate() {
        (ctl.progress)(26 + (looked * 58 / total) as u32);
        if !work.charge(cost::SQZ_OBJECT) {
            break;
        }
        let Some(job) = picture_job(doc, survey, uses, num, how) else { continue };
        let need = image::memory(&job);
        // A picture that would not fit in the memory lent for the pictures, even alone, is left as it is.
        if need > how.picture_memory {
            continue;
        }
        if !batch.is_empty() && (held.saturating_add(need) > how.picture_memory || batch.len() >= threads * 3) {
            results.extend(run_batch(std::mem::take(&mut batch), threads, work));
            held = 0;
        }
        held = held.saturating_add(need);
        batch.push((num, job));
    }
    results.extend(run_batch(batch, threads, work));
    if work.was_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut replaced = IdMap::new();
    let mut count = 0u32;
    let mut limit = false;
    for (num, result) in results {
        match result {
            Ok(Done { image: Some(stream), mask }) => {
                replaced.insert(num, Object::Stream(stream));
                if let Some((m, s)) = mask {
                    replaced.insert(m, Object::Stream(s));
                }
                count += 1;
            }
            Ok(_) => {}
            Err(Error::Limit(_)) => limit = true,
            Err(_) => {}
        }
    }
    if limit || work.is_over() {
        warnings.push(Warning("the pictures were not all looked at: the work compressing them asked for more than is allowed".to_string()));
    }
    Ok((replaced, count))
}

// --- the whole -------------------------------------------------------------------------------------------------------

/// Does the file's XMP (the catalog's `/Metadata`) say that it is PDF/A-1: `pdfaid:part` 1, as an element or an attribute?
fn says_pdfa1(doc: &Document) -> bool {
    let Ok(catalog) = doc.catalog() else { return false };
    let Some(Ok(Object::Stream(xmp))) = catalog.get("Metadata").map(|o| doc.resolve(o)) else { return false };
    let Ok(text) = doc.decode_stream_limited(&xmp, 4 << 20) else { return false };
    let mut from = 0;
    while let Some(at) = crate::lexer::find_bytes(&text, b"pdfaid:part", from) {
        let rest = text.get(at + 11..).unwrap_or(&[]);
        let mut value = rest.iter().skip_while(|b| matches!(b, b'>' | b'=' | b'"' | b'\'' | b' ' | b'\t' | b'\r' | b'\n'));
        if value.next() == Some(&b'1') && !value.next().is_some_and(u8::is_ascii_digit) {
            return true;
        }
        from = at + 1;
    }
    false
}

/// Compress the document. `password` is the one the file was opened with (it tells whether the owner opened it).
///
/// A file that is signed is refused (writing it again breaks the signature); so is one whose author did not allow changing its content
/// when the pictures are to be changed, unless the owner password opened it (as for putting a watermark on it): with `lossless_only`
/// only the signature matters. A file that says it is PDF/A-1 is written with a classic table. The input is not touched.
pub fn compress(doc: &Document, password: &str, how: &Options, ctl: &Control<'_>) -> Result<Packed> {
    // Only what changes the pictures needs the right to change the content: the lossless part is a plain rewrite, like rotating or taking
    // pages out, and keeps the protection the file has.
    if !how.lossless_only {
        crate::edit::content::check_content_allowed(doc, password)?;
    }
    ops::ensure_unsigned(doc)?;
    let loaded = ops::load(doc)?;
    let decoders = (how.picture_memory / thread_count(how) as u64).max(8 << 20);
    let work = Work::with_allowance(ALLOWANCE).with_cancel(ctl.stop.clone()).with_decoder_memory(decoders);
    (ctl.progress)(0);
    let found = survey(doc, &work, ctl)?;
    let mut warnings = Vec::new();
    let mut replaced = IdMap::new();
    let mut pictures = 0;
    if !how.lossless_only && !found.images.is_empty() {
        (ctl.progress)(23);
        let mut walk = Walk::new(doc, &found, &work);
        let pages = loaded.pages.len().max(1);
        let mut complete = true;
        for (i, page) in loaded.pages.iter().enumerate() {
            match walk.page(page) {
                Ok(()) => {}
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                // Out of work: what the pages after this one draw is not known, so no picture is changed on the strength of the rest.
                Err(_) => {
                    complete = false;
                    break;
                }
            }
            (ctl.progress)(23 + (i * 3 / pages) as u32);
        }
        if complete {
            (replaced, pictures) = shrink_pictures(doc, &found, &walk.uses, how, &work, ctl, &mut warnings)?;
        } else {
            warnings.push(Warning("the pages ask for more work than compressing reads, so the pictures were left as they are".to_string()));
        }
    }
    (ctl.progress)(86);
    let merged = u32::try_from(found.merged.len()).unwrap_or(u32::MAX);
    // A stream is inflated to be deflated again only up to a part of the memory lent for the pictures, and what is deflated is paid for.
    let limit = usize::try_from(how.picture_memory / 2).unwrap_or(usize::MAX).clamp(8 << 20, 256 << 20);
    let tidy = {
        let work = &work;
        move |dict: &mut Dict, data: &mut Vec<u8>| -> Result<()> {
            tidy_stream(dict, data, limit, &mut |bytes| spend(work, bytes as f64 * cost::SQZ_FLATE_BYTE)).map(|_| ())
        }
    };
    // A file that says it is PDF/A-1 is written with a classic table: that standard has no object streams (ISO 19005-1, 6.1.4).
    let pack = !says_pdfa1(doc);
    if !pack {
        warnings.push(Warning("the file says it is PDF/A-1, which has no object streams or cross-reference stream: it is written with a classic table".to_string()));
    }
    let picks: Vec<ops::Pick> = ops::all_picks(0, loaded.pages.len()).collect();
    loaded.size_hint.set(doc.data().len());
    let out = ops::build(&[loaded], &picks, Encrypt::Keep, Some(Tune { merged: found.merged, replaced, tidy: Box::new(tidy), pack, no_metadata: false }))?;
    (ctl.progress)(100);
    warnings.extend(out.warnings);
    let before = doc.data().len() as u64;
    let smaller = (out.data.len() as u64) < before;
    Ok(Packed {
        after: if smaller { out.data.len() as u64 } else { before },
        data: smaller.then_some(out.data),
        before,
        warnings,
        pictures,
        merged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    fn doc_with_catalog_metadata(xmp: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 4 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
        b.stream_obj(4, "/Type /Metadata /Subtype /XML", xmp.as_bytes());
        Document::from_bytes(b.finish_classic(5, "/Root 1 0 R")).unwrap()
    }

    #[test]
    fn matrix_entries_are_told_apart_by_ratio_not_by_difference() {
        // A form drawn at 0.01 and at 0.05 is drawn at two sizes; one drawn at 1.00 and 1.01 at one.
        assert_ne!(relative(0.01), relative(0.05));
        assert_ne!(relative(0.0001), relative(0.0002));
        assert_eq!(relative(1.0), relative(1.01));
        assert_eq!(relative(300.0), relative(301.0));
        assert_ne!(relative(300.0), relative(400.0));
        assert_ne!(relative(1.0), relative(-1.0));
        assert_eq!((relative(0.0), relative(f64::NAN), relative(f64::INFINITY)), (0, 0, 0));
        assert_ne!(relative(0.0), relative(1e-6), "a very small entry is not a zero");
    }

    #[test]
    fn a_page_may_run_as_many_operators_as_the_limit_says_whatever_the_pages_before_it_ran() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 5 0 R 7 0 R 9 0 R] /Count 4 >>");
        let image = "/Type /XObject /Subtype /Image /Width 40 /Height 40 /ColorSpace /DeviceGray /BitsPerComponent 8";
        for (page, content, count) in [(3, 4, 40), (5, 6, 40), (7, 8, 40), (9, 10, 90)] {
            b.obj(page, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents {content} 0 R /Resources << /XObject << /Im1 11 0 R >> >> >>"));
            // (`q Q` pairs: two operators each; then the picture.)
            let text = format!("{}q 50 0 0 50 0 0 cm /Im1 Do Q", "q Q ".repeat(count));
            b.stream_obj(content, "", text.as_bytes());
        }
        b.stream_obj(11, image, &[7u8; 1600]);
        let doc = Document::from_bytes(b.finish_classic(12, "/Root 1 0 R")).unwrap();
        let work = Work::new();
        let ctl = Control { stop: None, progress: &|_| {} };
        let found = survey(&doc, &work, &ctl).unwrap();
        let loaded = ops::load(&doc).unwrap();
        let mut walk = Walk::new(&doc, &found, &work);
        // 85 operators fit a page; three pages of 83 run one after the other (they would not, if the limit were for them all).
        walk.ops_limit = 85;
        for page in &loaded.pages[..3] {
            walk.page(page).unwrap();
        }
        // The fourth, of 185, does not.
        let e = walk.page(&loaded.pages[3]).unwrap_err();
        assert!(matches!(e, Error::Limit(_)), "{e}");
    }

    #[test]
    fn only_the_xmp_that_says_part_1_is_pdfa_1() {
        for (xmp, expected) in [
            ("<x:xmpmeta><rdf:Description xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"><pdfaid:part>1</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance></rdf:Description></x:xmpmeta>", true),
            ("<rdf:Description pdfaid:part=\"1\" pdfaid:conformance=\"A\"/>", true),
            ("<rdf:Description pdfaid:part='1'/>", true),
            ("<pdfaid:part> 1 </pdfaid:part>", true),
            ("<pdfaid:part>2</pdfaid:part>", false),
            ("<pdfaid:part>3</pdfaid:part>", false),
            ("<pdfaid:part>12</pdfaid:part>", false),
            ("<dc:title>part 1</dc:title>", false),
            ("", false),
        ] {
            assert_eq!(says_pdfa1(&doc_with_catalog_metadata(xmp)), expected, "{xmp}");
        }
        // No metadata at all.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        assert!(!says_pdfa1(&Document::from_bytes(b.finish_classic(3, "/Root 1 0 R")).unwrap()));
    }

    #[test]
    fn a_colour_transform_other_than_one_is_found_in_a_dictionary_or_an_array() {
        let parse = |decode_parms: &str| {
            let mut b = PdfBuilder::new();
            b.obj(1, &format!("<< /DecodeParms {decode_parms} >>"));
            let doc = Document::from_bytes(b.finish_classic(2, "/Root 1 0 R")).unwrap();
            let Object::Dict(d) = doc.get(ObjRef::new(1, 0)).unwrap() else { panic!() };
            d
        };
        assert!(colour_transform_is_off(&parse("<< /ColorTransform 0 >>")));
        assert!(colour_transform_is_off(&parse("[ << /Columns 4 >> << /ColorTransform 0 >> ]")));
        assert!(colour_transform_is_off(&parse("<< /ColorTransform 2 >>")));
        assert!(!colour_transform_is_off(&parse("<< /ColorTransform 1 >>")));
        assert!(!colour_transform_is_off(&parse("<< /Columns 4 >>")));
        assert!(!colour_transform_is_off(&parse("null")));
    }
}
