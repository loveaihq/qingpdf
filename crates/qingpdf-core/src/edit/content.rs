//! Text, pictures and watermarks written into the page itself (4c; ISO 32000-1 7.8.2, 8.4, 8.9, 9.4, 11.6.4.4).
//!
//! Unlike the annotations of 4a and 4b these are part of the page's content: printing, other readers and text extraction see them
//! like anything else on the page. Each addition is one *mark*, a form XObject that holds its own font, picture and transparency,
//! and that the page's content draws with `Do` (once, or once for each tile of a watermark). An addition on top of the page wraps
//! the page's own content in `q` ... `Q` (so that a graphics state it leaves unbalanced cannot reach the new content) and puts
//! the new content last; one underneath is put first, and balanced by itself. The page's content is read once (for each of its
//! streams) to count the `q` it leaves open and the `Q` that close nothing, so that exactly those are made up for; an open text
//! object (`BT`) or path is ended as well (8.4.2, 9.4.1, 8.5.2).
//!
//! What many pages share is made once: the mark (its font and picture too); the `q` stream; a page's own stream, for the pages that
//! need the same one (the same size); a `/Resources` that pages share or inherit is copied once for all of them, not once for
//! each page, and not changed where it is (that would change the pages that were not asked for); so is an `/XObject` dictionary
//! that is an object of its own.
//!
//! A watermark's drawing is marked as an artifact of the page, not as its content (14.8.2.2: `/Artifact` with `/Subtype
//! /Watermark`), so that text extraction leaves it out; added text and pictures are content.
//!
//! Places are the page as it is shown (the crop box turned by `/Rotate`); every matrix here ends with `upright`, which carries the
//! shown page's own space (origin at its lower left, y up) onto user space, so that text, pictures and watermarks stand upright
//! and a watermark is centred whatever the page's `/Rotate`, the origin of its boxes and (the engine does not scale by it) its
//! `/UserUnit`.

use std::collections::BTreeSet;
use std::fmt::Write;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::appearance::num;
use super::boxes::{self, BOX_COST, MAX_FONT_SIZE, MIN_FONT_SIZE};
use super::fontsub::{self, Look};
use crate::layout::{self, Face};
use super::signature::{self, Policy};
use super::{Ctx, Edit, EditClass, MAX_FREETEXT_CHARS, apply_metered, commit, replay, save_section};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::image;
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::render::work::Work;
use crate::text::scan::{Item, Scanner};
use crate::text::Shown;

/// The page that an edit of all the pages is said to be about (the reader draws every page again).
pub const ALL_PAGES: u32 = u32::MAX;
/// Most characters of a watermark's text, and most pages one watermark goes on.
pub const MAX_WATERMARK_CHARS: usize = 200;
pub const MAX_WATERMARK_PAGES: usize = 100_000;
/// What a page costs the work meter (units of about a nanosecond), besides the size of the page dictionary and resources it reads
/// and copies, per byte; a mark drawn; and a byte of a content stream decoded and scanned.
const PAGE_COST: f64 = 6_000.0;
const BYTE_COST: f64 = 8.0;
const MARK_COST: f64 = 2_500.0;
const SCAN_COST: f64 = 10.0;
/// The most `q` made up for in front of a page's content (for its stray `Q`) or `Q` after it (for the `q` it leaves open).
const MAX_BALANCE: u32 = 1_024;
/// The most bytes of one content stream that are decoded to be scanned.
const MAX_SCANNED: usize = 256 << 20;
/// Opens the marked content a watermark is drawn in (ISO 32000-1 14.8.2.2, Table 330); `EMC` closes it.
const WATERMARK_OPEN: &str = "/Artifact <</Type /Pagination /Subtype /Watermark>> BDC\n";
/// A page's tiles are chosen among at most this many places of a grid (the grid is made coarser until it has no more).
const MAX_CELLS: usize = 1_600;
/// What the command line lets an edit take, in bytes (the reader has a plan; a command has the memory of the machine).
const CLI_ROOM: usize = 512 << 20;
/// The em of a watermark's text and the height of its picture, before the mark is scaled to the page.
const MARK_EM: f64 = 100.0;
const MARK_GRAY: [u8; 3] = [128, 128, 128];
/// How many times a name for the mark is tried for in a page's resources.
const MAX_NAME_TRIES: usize = 32;
/// The most coordinates a place may have (points).
const LIMIT: f64 = 1.0e7;

type Matrix = [f64; 6];

/// A watermark: text, a picture or both (the picture above the text), drawn on every page of `pages`.
#[derive(Clone, Debug, PartialEq)]
pub struct Watermark {
    pub text: String,
    /// The bytes of a PNG or JPEG file, or nothing.
    pub image: Vec<u8>,
    /// More than 0 and at most 1 (11.6.4.4: `ca` and `CA`).
    pub opacity: f32,
    /// Degrees, counterclockwise as the page is seen.
    pub angle: f32,
    /// Repeated over the whole page instead of once in the middle.
    pub tile: bool,
    /// Under the page's own content instead of over it.
    pub under: bool,
    /// The pages, from 0.
    pub pages: Vec<u32>,
}

// --- matrices ----------------------------------------------------------------------------------------------------

/// `l` and then `m` (row vectors, as in 8.3.4).
fn mul(l: Matrix, m: Matrix) -> Matrix {
    [
        l[0] * m[0] + l[1] * m[2],
        l[0] * m[1] + l[1] * m[3],
        l[2] * m[0] + l[3] * m[2],
        l[2] * m[1] + l[3] * m[3],
        l[4] * m[0] + l[5] * m[2] + m[4],
        l[4] * m[1] + l[5] * m[3] + m[5],
    ]
}

/// The page as it is shown, with its origin at the lower left and y up, onto user space.
fn upright(shown: &Shown) -> Matrix {
    let (_, h) = shown.size();
    let p0 = shown.unmap(0.0, h);
    let pu = shown.unmap(1.0, h);
    let pv = shown.unmap(0.0, h - 1.0);
    [pu.0 - p0.0, pu.1 - p0.1, pv.0 - p0.0, pv.1 - p0.1, p0.0, p0.1]
}

/// The operator for a matrix. (A mark is made at about the size it has on the page, so the four decimals of `num` are enough for
/// its scale and turn.)
fn cm(m: Matrix) -> String {
    format!("{} {} {} {} {} {} cm", num(m[0]), num(m[1]), num(m[2]), num(m[3]), num(m[4]), num(m[5]))
}

fn charge(ctx: &Ctx<'_>, units: f64) -> Result<()> {
    if ctx.work.charge(units) { Ok(()) } else { Err(Error::Limit("the change took too much work".to_string())) }
}

/// A place or a size that is a number on the page.
fn place(v: f32) -> Result<f64> {
    super::finite_place(v)
}

// --- the page ------------------------------------------------------------------------------------------------------

/// What the pages of one edit share (small sorted lists: the keys are found by bisection).
#[derive(Default)]
struct Shared {
    /// Streams made, by their content.
    streams: Vec<(Vec<u8>, ObjRef)>,
    /// The copy made of a `/Resources` that pages share (its object number, or for one that the page tree gives them an address
    /// that is the same for all of them), with the name the mark has in it.
    resources: Vec<((bool, usize), String, ObjRef)>,
    /// The copy made of an `/XObject` dictionary that is an object of its own (by its number), with the name the mark has in it.
    xobjects: Vec<(u32, String, ObjRef)>,
    /// What the content streams (by object number) do to the graphics state, found once for each.
    balances: Vec<(u32, Balance)>,
    /// The page objects already changed (a page that the tree lists twice is changed once).
    done: Vec<u32>,
}

/// What a content stream (or several in a row) leaves behind: `Q` that close nothing in it, `q` still open, and whether a text
/// object or a path is open at the end (`None`: it has no word on that).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Balance {
    underflow: u32,
    depth: u32,
    text: Option<bool>,
    path: Option<bool>,
}

impl Balance {
    /// `self` and then `next`: the `Q` of `next` close the `q` that `self` left open first.
    fn then(self, next: Balance) -> Balance {
        let closed = self.depth.min(next.underflow);
        Balance {
            underflow: self.underflow.saturating_add(next.underflow - closed),
            depth: (self.depth - closed).saturating_add(next.depth),
            text: next.text.or(self.text),
            path: next.path.or(self.path),
        }
    }
}

/// Count the `q` and `Q` (8.4.2) of a content stream, and see whether it ends inside a text object (9.4.1) or a path (8.5.2).
/// Inline images are skipped, and what they hold is not read as operators.
fn scan_balance(data: &[u8]) -> Balance {
    let mut scanner = Scanner::new(data);
    let mut ops = Vec::new();
    let mut b = Balance::default();
    while let Item::Operator(op) = scanner.next(&mut ops) {
        match op {
            b"q" => b.depth = b.depth.saturating_add(1),
            b"Q" if b.depth > 0 => b.depth -= 1,
            b"Q" => b.underflow = b.underflow.saturating_add(1),
            b"BT" => b.text = Some(true),
            b"ET" => b.text = Some(false),
            b"m" | b"l" | b"c" | b"v" | b"y" | b"re" | b"h" => b.path = Some(true),
            b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => b.path = Some(false),
            b"BI" => {
                for _ in 0..4_096 {
                    match scanner.next(&mut ops) {
                        Item::Operator(b"ID") => {
                            scanner.skip_inline_image_data();
                            break;
                        }
                        Item::Operator(_) => {}
                        Item::End => return b,
                    }
                }
            }
            _ => {}
        }
    }
    b
}

/// The balance of one content stream, from the cache or read (decoded, scanned; charged by the byte). A stream that cannot be
/// read (a filter we do not have, a size over what the work left allows) says nothing: it counts as balanced.
fn stream_balance(ctx: &mut Ctx<'_>, shared: &mut Shared, r: ObjRef) -> Result<Balance> {
    let at = match shared.balances.binary_search_by(|(k, _)| k.cmp(&r.num)) {
        Ok(i) => return Ok(shared.balances.get(i).map_or_else(Balance::default, |b| b.1)),
        Err(i) => i,
    };
    let mut found = Balance::default();
    if let Ok(Object::Stream(s)) = ctx.doc.get(r) {
        charge(ctx, SCAN_COST * s.data.len() as f64)?;
        let limit = (ctx.work.remaining() / SCAN_COST).min(MAX_SCANNED as f64) as usize;
        let decoded = {
            let _own_budget = ctx.doc.page_decode_budget();
            ctx.doc.decode_stream_limited(&s, limit)
        };
        if let Ok(data) = decoded {
            charge(ctx, SCAN_COST * data.len() as f64)?;
            found = scan_balance(&data);
        }
    }
    shared.balances.insert(at, (r.num, found));
    Ok(found)
}

/// A stream with these bytes (zlib-compressed when that helps): the same bytes are one object.
fn stream(ctx: &mut Ctx<'_>, shared: &mut Shared, bytes: Vec<u8>) -> Result<ObjRef> {
    let at = match shared.streams.binary_search_by(|(k, _)| k.as_slice().cmp(bytes.as_slice())) {
        Ok(i) => return Ok(shared.streams.get(i).map_or(ObjRef::new(0, 0), |s| s.1)),
        Err(i) => i,
    };
    charge(ctx, 6.0 * bytes.len() as f64)?;
    let r = ctx.alloc()?;
    let mut dict = Dict::new();
    let data = if bytes.len() > 256 {
        dict.set("Filter", Object::from("FlateDecode"));
        compress_to_vec_zlib(&bytes, 6)
    } else {
        bytes.clone()
    };
    ctx.spend(bytes.len() + data.len() + 96)?;
    ctx.objects.push((r.num, Object::Stream(Stream { dict, data })));
    shared.streams.insert(at, (bytes, r));
    Ok(r)
}

/// The streams of a page's `/Contents` (7.7.3.3: one stream or an array of them, or nothing). A reference is looked at only far
/// enough to tell a stream from an array: the data of a page can be megabytes, and in an encrypted file reading it means decrypting it.
fn contents_list(doc: &Document, contents: Option<&Object>) -> Vec<Object> {
    let Some(mut at) = contents.cloned() else { return Vec::new() };
    for _ in 0..8 {
        match at {
            Object::Array(items) => return items.into_iter().filter(|o| matches!(o, Object::Ref(_))).collect(),
            Object::Ref(r) if doc.is_stream(r) => return vec![Object::Ref(r)],
            Object::Ref(r) => match doc.get(r) {
                Ok(next @ (Object::Array(_) | Object::Ref(_))) => at = next,
                _ => break,
            },
            _ => break,
        }
    }
    Vec::new()
}

/// A name for the mark that `xobjects` does not have yet: `wanted`, or that with `x` added.
/// (A dictionary that has them all, made on purpose, is not worth the time.)
fn free_name(xobjects: &Dict, wanted: &str) -> Result<String> {
    let mut name = wanted.to_string();
    for _ in 0..=MAX_NAME_TRIES {
        if !xobjects.contains_key(&name) {
            return Ok(name);
        }
        name.push('x');
    }
    Err(Error::Invalid("the page's resources have too many names like the one for the new mark".to_string()))
}

/// Put the mark `xobject` in the `/XObject` dictionary of `resources`, and give the name it has there. A dictionary of its own
/// (an object) is copied once, for all the resources that have it; one inside the resources is changed where it is.
fn add_xobject(ctx: &mut Ctx<'_>, shared: &mut Shared, resources: &mut Dict, wanted: &str, xobject: ObjRef) -> Result<String> {
    match resources.remove("XObject") {
        Some(Object::Ref(x)) => {
            let at = match shared.xobjects.binary_search_by(|(k, ..)| k.cmp(&x.num)) {
                Ok(i) => {
                    let (_, name, copy) = shared.xobjects.get(i).cloned().unwrap_or_else(|| (0, wanted.to_string(), x));
                    resources.set("XObject", Object::Ref(copy));
                    return Ok(name);
                }
                Err(i) => i,
            };
            let mut copy = match ctx.doc.resolve(&Object::Ref(x)) {
                Ok(Object::Dict(d)) => d,
                _ => Dict::new(),
            };
            charge(ctx, BYTE_COST * copy.approx_size() as f64)?;
            let name = free_name(&copy, wanted)?;
            copy.set(name.as_str(), Object::Ref(xobject));
            let r = ctx.alloc()?;
            ctx.spend(copy.approx_size() + 48)?;
            ctx.objects.push((r.num, Object::Dict(copy)));
            shared.xobjects.insert(at, (x.num, name.clone(), r));
            resources.set("XObject", Object::Ref(r));
            Ok(name)
        }
        other => {
            let mut xobjects = match other {
                Some(Object::Dict(d)) => d,
                _ => Dict::new(),
            };
            let name = free_name(&xobjects, wanted)?;
            xobjects.set(name.as_str(), Object::Ref(xobject));
            resources.set("XObject", Object::Dict(xobjects));
            Ok(name)
        }
    }
}

/// Make the page draw the mark `xobject`: `draw(name)` is the content that does it, `name` the one the mark has in the page's
/// resources (`wanted` unless the page has that already). On top of the page (its own content between `q` and `Q`), or `under` it.
fn put_on_page(ctx: &mut Ctx<'_>, shared: &mut Shared, page: &Page, wanted: &str, xobject: ObjRef, under: bool, draw: &dyn Fn(&str) -> String) -> Result<()> {
    let Object::Dict(mut dict) = ctx.doc.get(page.obj_ref)? else { return Err(Error::Invalid("the page is not a dictionary".to_string())) };
    charge(ctx, PAGE_COST + BYTE_COST * dict.approx_size() as f64)?;

    // The resources: a dictionary of the page's own is changed in place (the page is being written anyway); an object, or what the
    // page tree gives the page, is not changed: a copy is made, once for all the pages that have it (and only then is it read).
    let (key, inherited) = match dict.get("Resources") {
        Some(Object::Ref(r)) => (Some((false, r.num as usize)), None),
        Some(_) => (None, None),
        None => match page.inherited("Resources") {
            Some(a) => (Some((true, a.id)), Some(a.object)),
            None => (None, None),
        },
    };
    let known = key.and_then(|k| shared.resources.binary_search_by(|(have, ..)| have.cmp(&k)).ok());
    let (name, resources) = if let Some(i) = known.and_then(|i| shared.resources.get(i)) {
        (i.1.clone(), Object::Ref(i.2))
    } else {
        let from = match inherited {
            Some(o) => Some(o.clone()),
            None => dict.remove("Resources"),
        };
        let mut base = match from {
            Some(Object::Dict(d)) => d,
            Some(other) => match ctx.doc.resolve(&other) {
                Ok(Object::Dict(d)) => d,
                _ => Dict::new(),
            },
            None => Dict::new(),
        };
        charge(ctx, BYTE_COST * base.approx_size() as f64)?;
        let name = add_xobject(ctx, shared, &mut base, wanted, xobject)?;
        match key {
            Some(k) => {
                let r = ctx.alloc()?;
                ctx.spend(base.approx_size() + 48)?;
                ctx.objects.push((r.num, Object::Dict(base)));
                let at = shared.resources.binary_search_by(|(have, ..)| have.cmp(&k)).unwrap_or_else(|i| i);
                shared.resources.insert(at, (k, name.clone(), r));
                (name, Object::Ref(r))
            }
            None => (name, Object::Dict(base)),
        }
    };

    // The content: the page's own streams (kept as they are, wherever they are shared) between `q` and `Q`.
    let existing = contents_list(ctx.doc, dict.get("Contents"));
    let drawn = draw(&name).into_bytes();
    let mut list: Vec<Object> = Vec::with_capacity(existing.len() + 3);
    if under || existing.is_empty() {
        list.push(Object::Ref(stream(ctx, shared, drawn)?));
        if under {
            list.extend(existing);
        }
    } else {
        // What the page's content leaves open is closed after it, and what it closes without having opened it is opened before it
        // (that `Q` then closes ours): the new content is drawn in the state the page started in.
        let mut balance = Balance::default();
        for o in &existing {
            if let Some(r) = o.as_obj_ref() {
                balance = balance.then(stream_balance(ctx, shared, r)?);
            }
        }
        let opens = "q\n".repeat(1 + balance.underflow.min(MAX_BALANCE) as usize);
        let mut closes = String::new();
        if balance.path == Some(true) {
            closes.push_str("n\n");
        }
        if balance.text == Some(true) {
            closes.push_str("ET\n");
        }
        closes.push_str(&"Q\n".repeat(1 + balance.depth.min(MAX_BALANCE) as usize));
        list.push(Object::Ref(stream(ctx, shared, opens.into_bytes())?));
        list.extend(existing);
        let mut last = closes.into_bytes();
        last.extend_from_slice(&drawn);
        list.push(Object::Ref(stream(ctx, shared, last)?));
    }
    dict.set("Contents", Object::Array(list));
    dict.set("Resources", resources);
    ctx.spend(dict.approx_size() + 48)?;
    ctx.objects.push((page.obj_ref.num, Object::Dict(dict)));
    Ok(())
}

// --- marks ---------------------------------------------------------------------------------------------------------

/// What a mark shows: lines of text (`size` points high), a picture (`size` points high; above the text), at `opacity`.
struct Mark<'a> {
    lines: Vec<&'a str>,
    size: f64,
    color: [u8; 3],
    image: &'a [u8],
    opacity: f64,
}

/// A mark made: its form, the size of its box (the lower left corner is the origin of its own space), and the left end of the first
/// line's baseline in it.
struct Made {
    form: ObjRef,
    w: f64,
    h: f64,
    base: (f64, f64),
}

fn line_width(face: &dyn Face, line: &str) -> f64 {
    line.chars().map(|c| face.width(c)).sum::<f64>() / 1000.0
}

fn make_mark(ctx: &mut Ctx<'_>, m: &Mark<'_>) -> Result<Made> {
    let mut resources = Dict::new();
    let size = m.size;
    let chars: BTreeSet<char> = m.lines.iter().flat_map(|l| l.chars()).collect();
    let font = if chars.is_empty() {
        None
    } else {
        let font = fontsub::choose(&chars, Look::Sans)?;
        let embedded = fontsub::embed(&font, &chars, ctx.work)?;
        let from = ctx.objects.len();
        let font_ref = fontsub::add_objects(ctx, &embedded)?;
        ctx.tally(from)?;
        Some((embedded, font_ref))
    };
    let picture = if m.image.is_empty() {
        None
    } else {
        let from = ctx.objects.len();
        let p = boxes::picture(ctx, m.image)?;
        ctx.tally(from)?;
        Some(p)
    };
    // The size: the text (one line under another) and above it the picture, both in the middle.
    let (asc, desc) = font.as_ref().map_or((0.0, 0.0), |(e, _)| e.ascent_descent());
    let lead = (asc - desc) * size;
    let text_h = lead * m.lines.len() as f64;
    let text_w = font.as_ref().map_or(0.0, |(e, _)| m.lines.iter().map(|l| line_width(e, l)).fold(0.0, f64::max) * size);
    let gap = if font.is_some() && picture.is_some() { size * 0.2 } else { 0.0 };
    let image_w = picture.as_ref().map_or(0.0, |p| size * f64::from(p.width) / f64::from(p.height));
    let image_h = if picture.is_some() { size } else { 0.0 };
    let (w, h) = (text_w.max(image_w).max(1.0), (text_h + gap + image_h).max(1.0));

    let mut content = String::new();
    if m.opacity < 1.0 {
        let mut state = Dict::new();
        state.set("Type", Object::from("ExtGState"));
        state.set("ca", Object::Real(m.opacity));
        state.set("CA", Object::Real(m.opacity));
        let mut states = Dict::new();
        states.set("GS0", Object::Dict(state));
        resources.set("ExtGState", Object::Dict(states));
        content.push_str("/GS0 gs\n");
    }
    if let Some(p) = &picture {
        let mut o = image::orientation_matrix(p.orientation, image_w, image_h);
        o[4] += (w - image_w) / 2.0;
        o[5] += text_h + gap;
        let _ = writeln!(content, "q\n{} cm\n/Im0 Do\nQ", o.map(num).join(" "));
        let mut images = Dict::new();
        images.set("Im0", Object::Ref(p.image));
        resources.set("XObject", Object::Dict(images));
    }
    let mut base = (0.0, 0.0);
    if let Some((embedded, font_ref)) = &font {
        base = ((w - text_w) / 2.0, text_h - lead - desc * size);
        let rgb = m.color.map(|c| num(f64::from(c) / 255.0));
        let _ = write!(content, "BT\n/F0 {} Tf\n{} {} {} rg\n{} {} Td\n", num(size), rgb[0], rgb[1], rgb[2], num(base.0), num(base.1));
        for (k, line) in m.lines.iter().enumerate() {
            if k > 0 {
                let _ = writeln!(content, "0 {} Td", num(-lead));
            }
            let _ = writeln!(content, "{} Tj", embedded.encode(line));
        }
        content.push_str("ET\n");
        let mut fonts = Dict::new();
        fonts.set("F0", Object::Ref(*font_ref));
        resources.set("Font", Object::Dict(fonts));
    }
    charge(ctx, 6.0 * content.len() as f64)?;
    let form = ctx.alloc()?;
    let mut dict = Dict::new();
    dict.set("Type", Object::from("XObject"));
    dict.set("Subtype", Object::from("Form"));
    dict.set("BBox", super::reals(&[0.0, 0.0, w, h]));
    dict.set("Resources", Object::Dict(resources));
    dict.set("Filter", Object::from("FlateDecode"));
    let data = compress_to_vec_zlib(content.as_bytes(), 6);
    ctx.spend(data.len() + dict.approx_size() + 48)?;
    ctx.objects.push((form.num, Object::Stream(Stream { dict, data })));
    Ok(Made { form, w, h, base })
}

/// Make the mark and draw it on `pages`: `places` gives, for a page as it is shown, the matrices that carry the mark's space onto
/// the shown page's own (origin at its lower left, y up), one for each time the mark is drawn.
fn put_marks(ctx: &mut Ctx<'_>, pages: &[&Page], mark: &Mark<'_>, under: bool, watermark: bool, places: &dyn Fn(&Shown, &Made) -> Vec<Matrix>) -> Result<()> {
    let made = make_mark(ctx, mark)?;
    let name = format!("Qp{}", made.form.num);
    let mut shared = Shared::default();
    for page in pages {
        match shared.done.binary_search(&page.obj_ref.num) {
            Ok(_) => continue,
            Err(i) => shared.done.insert(i, page.obj_ref.num),
        }
        let shown = Shown::of(page);
        let to_user = upright(&shown);
        let each: Vec<Matrix> = places(&shown, &made).into_iter().map(|m| mul(m, to_user)).collect();
        charge(ctx, MARK_COST * each.len() as f64)?;
        put_on_page(ctx, &mut shared, page, &name, made.form, under, &|n| {
            let draws: String = each.iter().map(|m| format!("q {} /{n} Do Q\n", cm(*m))).collect();
            if watermark { format!("{WATERMARK_OPEN}{draws}EMC\n") } else { draws }
        })?;
    }
    Ok(())
}

/// `text` with the line ends of every kind as `\n` (or, with `one_line`, as spaces), control characters left out, cut at `most`
/// characters.
fn clean(text: &str, one_line: bool, most: usize) -> String {
    let mut out = String::new();
    let (mut after_cr, mut count) = (false, 0usize);
    for c in text.chars() {
        let ends = matches!(c, '\n' | '\r');
        if (c == '\n' && after_cr) || (!ends && c.is_control()) {
            after_cr = false;
            continue;
        }
        after_cr = c == '\r';
        out.push(if ends && one_line { ' ' } else if ends { '\n' } else { c });
        count += 1;
        if count >= most {
            break;
        }
    }
    out
}

fn translate(x: f64, y: f64) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

// --- text, a picture --------------------------------------------------------------------------------------------------

/// Text on the page: the first line's baseline starts at `at` (points of the page as shown, from the top left).
pub(super) fn add_text(ctx: &mut Ctx<'_>, page: &Page, at: [f32; 2], text: &str, size: f32, color: [u8; 3]) -> Result<()> {
    let size = f64::from(size);
    if !size.is_finite() {
        return Err(Error::Invalid("the size of the text is not a number".to_string()));
    }
    let (x, y) = (place(at[0])?, place(at[1])?);
    let text = clean(text, false, MAX_FREETEXT_CHARS);
    if text.trim().is_empty() {
        return Err(Error::Invalid("there is no text".to_string()));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > layout::MAX_LINES {
        return Err(Error::Limit(format!("the text takes more than {} lines", layout::MAX_LINES)));
    }
    charge(ctx, BOX_COST + 600.0 * text.chars().count() as f64)?;
    let mark = Mark { lines, size: size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE), color, image: &[], opacity: 1.0 };
    put_marks(ctx, &[page], &mark, false, false, &|shown, made| vec![translate(x - made.base.0, shown.size().1 - y - made.base.1)])
}

/// A picture on the page, as big as fits in `rect` with its own shape, in the middle of it.
pub(super) fn add_image(ctx: &mut Ctx<'_>, page: &Page, rect: [f32; 4], bytes: &[u8]) -> Result<()> {
    let [a, b, c, d] = [place(rect[0])?, place(rect[1])?, place(rect[2])?, place(rect[3])?];
    let (l, r, t, bt) = (a.min(c), a.max(c), b.min(d), b.max(d));
    if r - l < 1.0e-3 || bt - t < 1.0e-3 {
        return Err(Error::Invalid("the place of the picture has no area".to_string()));
    }
    let mark = Mark { lines: Vec::new(), size: MARK_EM, color: [0; 3], image: bytes, opacity: 1.0 };
    put_marks(ctx, &[page], &mark, false, false, &|shown, made| {
        let scale = ((r - l) / made.w).min((bt - t) / made.h);
        let (left, top) = (l + ((r - l) - made.w * scale) / 2.0, t + ((bt - t) - made.h * scale) / 2.0);
        vec![[scale, 0.0, 0.0, scale, left, shown.size().1 - top - made.h * scale]]
    })
}

// --- watermarks ---------------------------------------------------------------------------------------------------------

/// How a mark of `mark_w` by `mark_h` (its own units) goes on a page of `w` by `h` points: the scale, and the centres of the marks
/// (one in the middle, or a staggered grid turned with the mark, all the places of it that touch the page).
fn arrange(w: f64, h: f64, mark_w: f64, mark_h: f64, angle: f64, tile: bool) -> (f64, Vec<(f64, f64)>) {
    let (sin, cos) = angle.to_radians().sin_cos();
    // The longest line through the middle of the page at that angle: the mark is made to fit in it (and in the height of the page).
    let chord = [if cos.abs() > 1.0e-9 { w / cos.abs() } else { f64::INFINITY }, if sin.abs() > 1.0e-9 { h / sin.abs() } else { f64::INFINITY }].into_iter().fold(f64::INFINITY, f64::min);
    let (fit, cap) = if tile { (0.5, 0.055) } else { (0.7, 0.2) };
    let scale = (fit * chord / mark_w).min(cap * w.min(h) / mark_h).max(1.0e-3);
    let (mw, mh) = (mark_w * scale, mark_h * scale);
    let centre = (w / 2.0, h / 2.0);
    if !tile {
        return (scale, vec![centre]);
    }
    let (mut px, mut py) = (1.6 * mw + mh, 3.5 * mh);
    let reach = w.hypot(h) / 2.0 + mw.hypot(mh) / 2.0;
    let cells = |px: f64, py: f64| {
        let (nx, ny) = ((reach / px).ceil() as usize + 1, (reach / py).ceil() as usize + 1);
        (nx, ny, (2 * nx + 1).saturating_mul(2 * ny + 1))
    };
    while cells(px, py).2 > MAX_CELLS {
        px *= 1.25;
        py *= 1.25;
    }
    let (nx, ny, _) = cells(px, py);
    let (hx, hy) = ((mw * cos.abs() + mh * sin.abs()) / 2.0, (mw * sin.abs() + mh * cos.abs()) / 2.0);
    let mut centres = Vec::new();
    for j in -(ny as i64)..=(ny as i64) {
        let shift = if j.rem_euclid(2) == 1 { px / 2.0 } else { 0.0 };
        for i in -(nx as i64)..=(nx as i64) {
            let (u, v) = (i as f64 * px + shift, j as f64 * py);
            let (x, y) = (centre.0 + u * cos - v * sin, centre.1 + u * sin + v * cos);
            if x + hx > 0.0 && x - hx < w && y + hy > 0.0 && y - hy < h {
                centres.push((x, y));
            }
        }
    }
    (scale, centres)
}

/// The watermark on the pages `mark.pages`.
pub(super) fn watermark(ctx: &mut Ctx<'_>, pages: &[Page], mark: &Watermark) -> Result<()> {
    if mark.text.chars().count() > MAX_WATERMARK_CHARS {
        return Err(Error::Limit(format!("the text of a watermark has at most {MAX_WATERMARK_CHARS} characters")));
    }
    let text = clean(&mark.text, true, MAX_WATERMARK_CHARS);
    let text = text.trim();
    if text.is_empty() && mark.image.is_empty() {
        return Err(Error::Invalid("a watermark needs text or a picture".to_string()));
    }
    let opacity = f64::from(mark.opacity);
    if !(opacity > 0.0 && opacity <= 1.0) {
        return Err(Error::Invalid("the opacity of a watermark is more than 0 and at most 1".to_string()));
    }
    let angle = f64::from(mark.angle);
    if !angle.is_finite() {
        return Err(Error::Invalid("the angle of a watermark is not a number".to_string()));
    }
    if mark.pages.is_empty() {
        return Err(Error::Invalid("the watermark is for no page".to_string()));
    }
    if mark.pages.len() > MAX_WATERMARK_PAGES {
        return Err(Error::Limit(format!("a watermark goes on at most {MAX_WATERMARK_PAGES} pages at once")));
    }
    // (A page named twice, or listed twice by the page tree, is done once: `put_marks` keeps the page objects it has done.)
    let chosen: Vec<&Page> = mark.pages.iter().filter_map(|&i| pages.get(i as usize)).collect();
    if chosen.len() != mark.pages.len() {
        return Err(Error::Invalid("there is no such page".to_string()));
    }
    charge(ctx, BOX_COST + 600.0 * text.chars().count() as f64)?;
    let lines = if text.is_empty() { Vec::new() } else { vec![text] };
    let spec = Mark { lines, size: MARK_EM, color: MARK_GRAY, image: &mark.image, opacity };
    put_marks(ctx, &chosen, &spec, mark.under, true, &|shown, made| {
        let (w, h) = shown.size();
        let (scale, centres) = arrange(w, h, made.w, made.h, angle, mark.tile);
        let (sin, cos) = angle.to_radians().sin_cos();
        let (a, b, c, d) = (scale * cos, scale * sin, -scale * sin, scale * cos);
        // The mark's own point (u, v) goes to the centre plus (u - w/2, v - h/2), scaled and turned.
        centres.into_iter().map(|(x, y)| [a, b, c, d, x - (a * made.w + c * made.h) / 2.0, y - (b * made.w + d * made.h) / 2.0]).collect()
    })
}

// --- the command line -------------------------------------------------------------------------------------------------

/// What the command line asks for. Places are points from the lower left corner of the page as it is shown (to the right and up).
#[derive(Clone, Debug, PartialEq)]
pub enum ContentOp {
    /// Text, the first line's baseline starting at `at`.
    Text { page: usize, at: [f64; 2], size: f64, text: String, color: [u8; 3] },
    /// A picture with its lower left corner at `at`, `width` points wide.
    Image { page: usize, at: [f64; 2], width: f64, image: Vec<u8> },
    Watermark(Watermark),
}

/// The file with the change appended to it.
#[derive(Debug)]
pub struct Added {
    /// The bytes of the new file: the whole original and then the incremental update. When the file's structure was damaged it
    /// was written afresh first (`rewritten`), and the original bytes are not in the result.
    pub data: Vec<u8>,
    pub rewritten: bool,
    /// Things the person should know about what was asked for: a size that was changed to the nearest one allowed, a place that is
    /// off the page.
    pub warnings: Vec<String>,
}

/// Why a file may not have the content of its pages changed, if it may not: its permissions (bit 4, unless it was opened with the
/// owner password) and its signatures.
pub fn check_content_allowed(doc: &Document, password: &str) -> Result<()> {
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    if let Some(encryption) = doc.encryption()
        && !encryption.permissions.modify
        && !crate::ops::has_owner_rights(doc, &encryption, password)
    {
        return Err(Error::Invalid(
            "the file's author did not allow changing its content (permission bit 4); the owner password is needed (give it with --password)".to_string(),
        ));
    }
    match signature::look(doc).policy(EditClass::Content) {
        Policy::Refuse(why) => Err(Error::Invalid(why.to_string())),
        _ => Ok(()),
    }
}

/// Add what `op` says to the file `doc` and give the bytes of the result (the file as it was, then one incremental update).
pub fn add_content(doc: &Document, password: &str, op: &ContentOp) -> Result<Added> {
    check_content_allowed(doc, password)?;
    // A file whose cross-reference data is damaged cannot be added to: it is written afresh, as a whole, first.
    let rewritten = doc.was_repaired() || doc.leading_junk() > 0;
    let fresh = if rewritten { Some(Document::from_bytes_with_password(crate::ops::copy_all(doc)?.data, password)?) } else { None };
    let doc = fresh.as_ref().unwrap_or(doc);
    let pages = doc.pages()?;
    let size_of = |index: usize| pages.get(index).map(|p| Shown::of(p).size()).ok_or_else(|| Error::Invalid(format!("there is no page {} (the file has {})", index + 1, pages.len())));
    let number = |v: f64, what: &str| if v.is_finite() && v.abs() <= LIMIT { Ok(v) } else { Err(Error::Invalid(format!("{what} is not a number on the page"))) };
    let mut warnings = Vec::new();
    // A place that is not on the page (the text or the picture is cut off by the page, or not seen at all).
    let mut off_page = |page: usize, (x, y): (f64, f64), (w, h): (f64, f64)| {
        if x < 0.0 || y < 0.0 || x > w || y > h {
            warnings.push(format!("the place {x},{y} is off page {} ({w:.0} by {h:.0} points); what is outside the page is not seen", page + 1));
        }
    };
    let edit = match op {
        ContentOp::Text { page, at, size, text, color } => {
            let (w, height) = size_of(*page)?;
            let (x, y) = (number(at[0], "the place")?, number(at[1], "the place")?);
            let wanted = if size.is_finite() { *size } else { return Err(Error::Invalid("the size is not a number".to_string())) };
            let size = wanted.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
            off_page(*page, (x, y), (w, height));
            if size != wanted {
                let said = if wanted.abs() < LIMIT { wanted.to_string() } else { format!("{wanted:e}") };
                warnings.push(format!("the size {said} was changed to {size} (text is from {MIN_FONT_SIZE} to {MAX_FONT_SIZE} points high)"));
            }
            Edit::AddText { page: *page as u32, at: [x as f32, (height - y) as f32], text: text.clone(), size: size as f32, color: *color }
        }
        ContentOp::Image { page, at, width, image } => {
            let (page_w, height) = size_of(*page)?;
            let (x, y, width) = (number(at[0], "the place")?, number(at[1], "the place")?, number(*width, "the width")?);
            if width <= 0.0 {
                return Err(Error::Invalid("the width of the picture must be more than 0".to_string()));
            }
            let (pw, ph, _) = image::probe(image)?;
            let tall = width * f64::from(ph) / f64::from(pw.max(1));
            off_page(*page, (x, y), (page_w, height));
            Edit::AddImage { page: *page as u32, rect: [x as f32, (height - y - tall) as f32, (x + width) as f32, (height - y) as f32], image: image.clone() }
        }
        ContentOp::Watermark(mark) => Edit::Watermark(Box::new(mark.clone())),
    };
    let work = Work::new();
    let objects = apply_metered(doc, &pages, &edit, "", &work, CLI_ROOM)?;
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
    fn matrices_carry_the_shown_page_onto_user_space_whatever_its_turn_and_box() {
        let page = |extra: &str| {
            let mut b = crate::testutil::PdfBuilder::new();
            b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
            b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
            b.obj(3, &format!("<< /Type /Page /Parent 2 0 R {extra} >>"));
            let doc = Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap();
            Shown::of(&doc.pages().unwrap()[0])
        };
        // 300 x 200 with its lower left at (50, 30): the shown page's lower left is there, x runs right, y up.
        assert_eq!(upright(&page("/MediaBox [50 30 350 230]")), [1.0, 0.0, 0.0, 1.0, 50.0, 30.0]);
        // Turned a quarter, a half and three quarters clockwise: the shown page's lower left is the corner of the page that ends up there.
        assert_eq!(upright(&page("/MediaBox [0 0 300 200] /Rotate 90")), [0.0, 1.0, -1.0, 0.0, 300.0, 0.0]);
        assert_eq!(upright(&page("/MediaBox [0 0 300 200] /Rotate 180")), [-1.0, 0.0, 0.0, -1.0, 300.0, 200.0]);
        assert_eq!(upright(&page("/MediaBox [0 0 300 200] /Rotate 270")), [0.0, -1.0, 1.0, 0.0, 0.0, 200.0]);
    }

    #[test]
    fn one_mark_is_in_the_middle_and_tiles_cover_the_page_within_the_limit() {
        let (scale, one) = arrange(612.0, 792.0, 200.0, 120.0, 45.0, false);
        assert_eq!(one, vec![(306.0, 396.0)]);
        // The mark fits the page: its width and height, scaled, stay in the longest line through the middle and a fifth of the short side.
        assert!(200.0 * scale <= 0.7 * 612.0 / 45f64.to_radians().cos() + 1e-6 && 120.0 * scale <= 0.2 * 612.0 + 1e-6, "{scale}");
        let (_, many) = arrange(612.0, 792.0, 200.0, 120.0, 45.0, true);
        assert!(many.len() > 8 && many.len() < MAX_CELLS, "{}", many.len());
        assert!(many.contains(&(306.0, 396.0)), "a mark in the middle");
        // A huge page with a tiny mark does not make millions of places.
        let (_, huge) = arrange(14_400.0, 14_400.0, 5.0, 100.0, 0.0, true);
        assert!(huge.len() <= MAX_CELLS, "{}", huge.len());
    }

    #[test]
    fn an_edit_that_would_take_more_than_the_room_is_refused_while_it_is_made_not_after() {
        // 3000 pages with 10 KB in each page dictionary: a mark on every one rewrites 30 MB of page dictionaries, over the 16 MiB the
        // reader sets aside for what is not saved.
        let junk = "x".repeat(10_000);
        let mut b = crate::testutil::PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..3_000).map(|i| format!("{} 0 R ", 10 + i)).collect();
        b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 3000 /MediaBox [0 0 100 100] >>"));
        for i in 0..3_000 {
            b.obj(10 + i, &format!("<< /Type /Page /Parent 2 0 R /Junk ({junk}) >>"));
        }
        let doc = Document::from_bytes(b.finish_classic(3_010, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 4, 4);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&[200u8; 48]).unwrap();
        let mark = |count: u32| Edit::Watermark(Box::new(Watermark { text: String::new(), image: png.clone(), opacity: 0.5, angle: 0.0, tile: false, under: false, pages: (0..count).collect() }));
        let started = std::time::Instant::now();
        let err = super::super::apply(&doc, &pages, &mark(3_000), "").expect_err("refused").to_string();
        assert!(err.contains("too big") || err.contains("too much"), "{err}");
        assert!(started.elapsed().as_secs() < 10, "it took {:?}", started.elapsed());
        // Fewer pages fit, and nothing was left behind by the refused one.
        assert!(!doc.has_overlay());
        let made = super::super::apply(&doc, &pages, &mark(1_000), "").unwrap();
        assert!(made.iter().map(|(_, o)| o.approx_size()).sum::<usize>() < 16 << 20);
        assert_eq!(made.iter().filter(|(n, _)| (10..3_010).contains(n)).count(), 1_000, "one new page dictionary for each page asked for");
    }

    #[test]
    fn a_name_that_the_page_has_already_is_not_used_for_the_mark() {
        // The resources already have a lot of names like the mark's: it gets one of its own, and a page that has them all is refused.
        let names = |n: usize| -> String { (0..n).map(|i| format!("/Qp{i} 4 0 R ")).collect() };
        let file = |taken: &str| {
            let mut b = crate::testutil::PdfBuilder::new();
            b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
            b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
            b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << {taken} >> >> >>"));
            b.obj(4, "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] >>");
            Document::from_bytes(b.finish_classic(30, "/Root 1 0 R")).unwrap()
        };
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 2, 2);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&[9u8; 12]).unwrap();
        let mark = Edit::Watermark(Box::new(Watermark { text: String::new(), image: png, opacity: 1.0, angle: 0.0, tile: false, under: false, pages: vec![0] }));
        // Every number the mark's object could have (30 to 40) is taken as a name.
        let doc = file(&names(41));
        let made = super::super::apply(&doc, &doc.pages().unwrap(), &mark, "").unwrap();
        let Some((_, Object::Dict(page))) = made.iter().find(|(n, _)| *n == 3) else { panic!("the page was not written") };
        let Some(Object::Dict(res)) = page.get("Resources") else { panic!("{page:?}") };
        let Some(Object::Dict(xobjects)) = res.get("XObject") else { panic!("{res:?}") };
        assert_eq!(xobjects.len(), 41 + 1, "all the old names stay and one is added");
        assert!(xobjects.iter().any(|(k, v)| k.as_bytes().ends_with(b"x") && v.as_obj_ref().is_some_and(|r| r.num >= 30)), "{xobjects:?}");
        // A page that has all the names the tries would reach is refused, not looped over.
        let full: String = (0..40).flat_map(|i| (0..40).map(move |k| format!("/Qp{i}{} 4 0 R ", "x".repeat(k)))).collect();
        let doc = file(&full);
        let r = super::super::apply(&doc, &doc.pages().unwrap(), &mark, "");
        assert!(r.is_err_and(|e| e.to_string().contains("too many names")));
    }

    #[test]
    fn the_scan_counts_what_a_stream_leaves_open_and_what_it_closes_without_opening_it() {
        let scan = |s: &str| scan_balance(s.as_bytes());
        let b = |underflow, depth, text, path| Balance { underflow, depth, text, path };
        assert_eq!(scan("q 1 0 0 rg q Q"), b(0, 1, None, None));
        assert_eq!(scan("Q Q q"), b(2, 1, None, None));
        assert_eq!(scan("BT /F1 12 Tf (x) Tj"), b(0, 0, Some(true), None));
        assert_eq!(scan("BT (x) Tj ET BT"), b(0, 0, Some(true), None));
        assert_eq!(scan("BT (x) Tj ET"), b(0, 0, Some(false), None));
        assert_eq!(scan("10 10 m 20 20 l"), b(0, 0, None, Some(true)));
        assert_eq!(scan("10 10 m 20 20 l S 0 0 5 5 re W n"), b(0, 0, None, Some(false)));
        assert_eq!(scan("0 0 5 5 re W"), b(0, 0, None, Some(true)));
        // Inline image data is not read as operators; a comment and a string with the letters are not either.
        assert_eq!(scan("q BI /W 1 /H 1 /BPC 8 /CS /G ID Q Q Q\nEI q (Q) Tj % Q\n"), b(0, 2, None, None));
        // Streams one after the other: the `Q` of the second close the `q` of the first; flags are the last word on them.
        let (first, second) = (scan("q q BT"), scan("ET Q Q Q 1 1 m"));
        assert_eq!(first.then(second), b(1, 0, Some(false), Some(true)));
        assert_eq!(second.then(first), b(3, 2, Some(true), Some(true)));
        assert_eq!(Balance::default().then(first), first);
        assert_eq!(first.then(Balance::default()), first);
    }

    #[test]
    fn contents_are_listed_for_a_stream_an_array_a_reference_to_an_array_and_nothing() {
        let mut b = crate::testutil::PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
        b.obj(4, "[ 5 0 R 6 0 R null ]");
        b.stream_obj(5, "", b"0 g");
        b.stream_obj(6, "", b"1 g");
        b.obj(7, "7 0 R");
        let doc = Document::from_bytes(b.finish_classic(10, "/Root 1 0 R")).unwrap();
        let refs = |o: Option<Object>| contents_list(&doc, o.as_ref()).iter().filter_map(|o| o.as_obj_ref().map(|r| r.num)).collect::<Vec<_>>();
        assert_eq!(refs(Some(Object::Ref(ObjRef::new(5, 0)))), vec![5]);
        assert_eq!(refs(Some(Object::Ref(ObjRef::new(4, 0)))), vec![5, 6]);
        assert_eq!(refs(Some(Object::Array(vec![Object::Ref(ObjRef::new(5, 0)), Object::Null]))), vec![5]);
        assert_eq!(refs(None), Vec::<u32>::new());
        // A reference to itself, to nothing and to a dictionary is no content.
        assert_eq!(refs(Some(Object::Ref(ObjRef::new(7, 0)))), Vec::<u32>::new());
        assert_eq!(refs(Some(Object::Ref(ObjRef::new(9, 0)))), Vec::<u32>::new());
        assert_eq!(refs(Some(Object::Ref(ObjRef::new(3, 0)))), Vec::<u32>::new());
        assert!(doc.is_stream(ObjRef::new(5, 0)) && !doc.is_stream(ObjRef::new(4, 0)) && !doc.is_stream(ObjRef::new(3, 0)));
    }
}

#[cfg(test)]
mod calibration {
    use super::*;

    /// What the work meter is charged for a mark on many pages, next to the time it really took (units are about a nanosecond).
    #[test]
    fn the_meter_is_charged_in_proportion_to_the_time_a_watermark_takes() {
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 200, 200);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&[200u8; 200 * 200 * 4]).unwrap();
        // Pages that share a big resource dictionary (400 fonts), with a content stream each; and pages that each have their own.
        for (name, shared) in [("shared resources", true), ("own resources", false)] {
            let mut b = crate::testutil::PdfBuilder::new();
            b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
            let kids: String = (0..2_000).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
            let fonts: String = (0..400).map(|i| format!("/F{i} 5 0 R ")).collect();
            b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 2000 /MediaBox [0 0 300 400] >>"));
            b.obj(3, &format!("<< /Font << {fonts} >> >>"));
            b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
            for i in 0..2_000u32 {
                let resources = if shared { "3 0 R".to_string() } else { format!("<< /Font << {fonts} >> >>") };
                b.obj(10 + 2 * i, &format!("<< /Type /Page /Parent 2 0 R /Resources {resources} /Contents {} 0 R >>", 11 + 2 * i));
                b.stream_obj(11 + 2 * i, "", format!("BT /F1 12 Tf 20 20 Td (page {i}) Tj ET").as_bytes());
            }
            let doc = Document::from_bytes(b.finish_classic(10 + 4_000, "/Root 1 0 R")).unwrap();
            let pages = doc.pages().unwrap();
            for count in [100u32, 2_000] {
                let mark = Edit::Watermark(Box::new(Watermark { text: String::new(), image: png.clone(), opacity: 0.3, angle: 45.0, tile: true, under: false, pages: (0..count).collect() }));
                let work = Work::new();
                let started = std::time::Instant::now();
                let objects = apply_metered(&doc, &pages, &mark, "", &work, 1 << 30).unwrap();
                let took = started.elapsed().as_nanos() as f64;
                println!("{name}, {count} pages: {} objects, charged {:.0} units, took {took:.0} ns (charged / took = {:.2})", objects.len(), work.used(), work.used() / took);
                // (Measured 0.85 to 1.3 on the machine the constants were set on; the bounds leave room for a slow or a fast one.)
                if !cfg!(debug_assertions) {
                    assert!((0.15..=5.0).contains(&(work.used() / took)), "{name}, {count} pages: the meter is charged {:.0} units for {took:.0} ns", work.used());
                }
            }
        }
        // Bare pages (no resources, no content, a mark that is drawn once): what a page costs by itself.
        let mut b = crate::testutil::PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..25_000).map(|i| format!("{} 0 R ", 10 + i)).collect();
        b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 25000 /MediaBox [0 0 300 400] >>"));
        for i in 0..25_000u32 {
            b.obj(10 + i, "<< /Type /Page /Parent 2 0 R >>");
        }
        let doc = Document::from_bytes(b.finish_classic(10 + 25_000, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        let mark = Edit::Watermark(Box::new(Watermark { text: String::new(), image: png, opacity: 0.3, angle: 0.0, tile: false, under: false, pages: (0..25_000).collect() }));
        let work = Work::new();
        let started = std::time::Instant::now();
        let objects = apply_metered(&doc, &pages, &mark, "", &work, 1 << 30).unwrap();
        let took = started.elapsed().as_nanos() as f64;
        println!("bare pages, 25000 pages: {} objects, charged {:.0} units, took {took:.0} ns (charged / took = {:.2})", objects.len(), work.used(), work.used() / took);
        if !cfg!(debug_assertions) {
            assert!((0.15..=5.0).contains(&(work.used() / took)), "bare pages: the meter is charged {:.0} units for {took:.0} ns", work.used());
        }
    }
}
