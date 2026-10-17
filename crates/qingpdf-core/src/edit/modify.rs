//! Changing an annotation that is already on the page (2026-10-17), ours or another program's: moving it, resizing it, changing its
//! colour or the width of its line, changing its text. The annotation is found by its place in the page's list and its object number, as
//! for taking it out; the changed object (and the new appearance if there is one) go in front of the document's own like every edit, so
//! all of it is undone in one step and saved in the one update.
//!
//! - **Moving** changes `/Rect` and the points of the annotation (`/QuadPoints`, `/InkList`, `/L`, `/Vertices`, `/CL`, the rectangle of its
//!   popup) by the same distance. The appearance is kept: the form's box is put onto the new `/Rect` (Algorithm 8.1), so it follows.
//! - **Resizing** puts the annotation in a new rectangle. What the reader can draw again (squares, circles, lines, drawings and text
//!   boxes) gets a new appearance of our own, so the lines keep their width and the text its size; pictures and the shapes of other
//!   programs the reader cannot draw (polygons, poly-lines) keep their appearance, which Algorithm 8.1 stretches over the new `/Rect`.
//! - **Recolouring** and **a new line width** draw the appearance again, for the kinds the reader draws: text markup, drawings, shapes,
//!   notes and text boxes. Any other kind is refused.
//! - **A new text** is the annotation's `/Contents`; a text box is drawn again with it.
//!
//! A locked annotation (flag bit 8) or a read-only one (bit 7) is not changed at all (a locked one may still be given a new text), and one whose
//! contents are locked (bit 10) keeps its text. Annotations that are part of the page (links, form fields, popups) are not touched here.

use super::appearance::{self, Quad};
use super::boxes::{self, Fit};
use super::shapes::{self, End, Look};
use super::{Ctx, FLAG_LOCKED, MAX_NOTE_CHARS, MAX_QUADS, MarkupKind, finite_place, locate, reals, text_string, unit, with_appearance};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::info::decode_text_lines;
use crate::idmap::IdSet;
use crate::object::{Dict, ObjRef, Object, PdfString};
use crate::text::Shown;

/// Bit 7 of the flags, ReadOnly; bit 5, NoRotate; bit 10, LockedContents (Table 165; Locked, bit 8, is `FLAG_LOCKED`).
const FLAG_READONLY: i64 = 64;
const FLAG_NO_ROTATE: i64 = 16;
const FLAG_LOCKED_CONTENTS: i64 = 512;
/// Most numbers of the points of one annotation that are changed (a drawing of 50,000 points), and what changing one costs the meter.
const MAX_GEOMETRY: usize = 100_000;
const NUMBER_COST: f64 = 8.0;
/// What is said of an annotation that has what the reader cannot draw again without losing it.
const CANNOT_AGAIN: &str = "it has something the reader cannot draw again without losing it (a dash pattern, rich text, a border or fill of a text box, a callout, a cloud, a leader line, a caption, another line end or note icon, curves, an opacity on a text box)";
/// The largest number a PDF holds (Annex C.2): a point farther out than this is not drawn, and not written.
const REAL_LIMIT: f64 = 3.4e38;
/// What drawing an annotation again costs the work meter (a text box is charged for its text and font besides).
const REDRAW_COST: f64 = 20_000.0;
/// The most of the page's width a box may keep off the page when it is moved, and the least that stays on it, in points.
const KEEP_ON_PAGE: f64 = 8.0;

/// The errors of this module, made in one place each (a call is shorter than a string and its copy).
#[inline(never)]
pub(super) fn bad(m: &'static str) -> Error {
    Error::Invalid(m.to_string())
}

#[inline(never)]
pub(super) fn too_much(m: &'static str) -> Error {
    Error::Limit(m.to_string())
}

/// Put an array of reals in the dictionary.
#[inline(never)]
pub(super) fn put_reals(d: &mut Dict, key: &str, v: &[f64]) {
    d.set(key, reals(v));
}

/// The kinds of annotation that are part of the page, not annotations a person changes.
fn part_of_page(sub: &[u8]) -> bool {
    matches!(sub, b"Link" | b"Widget" | b"Popup")
}

/// The annotation the person picked: its object and dictionary, and its type.
struct Found {
    r: ObjRef,
    d: Dict,
    sub: Vec<u8>,
    /// The page's list of annotations as it is now.
    items: Vec<Object>,
}

fn find(ctx: &Ctx<'_>, pages: &[Page], page: &Page, index: u32, num: u32) -> Result<Found> {
    let gone = || bad("the annotation is not there any more");
    let Object::Dict(page_dict) = ctx.doc.get(page.obj_ref)? else { return Err(gone()) };
    let (items, _) = locate(ctx.doc, pages, page, &page_dict)?;
    let item = items.get(index as usize).ok_or_else(gone)?;
    let Object::Ref(r) = item else { return Err(bad("an annotation written inside the page's own list cannot be changed")) };
    let r = *r;
    if num == 0 || r.num != num {
        return Err(gone());
    }
    let Object::Dict(d) = ctx.doc.get(r)? else { return Err(gone()) };
    let sub = match d.get("Subtype").map(|s| ctx.doc.resolve(s)) {
        Some(Ok(Object::Name(n))) => n.as_bytes().to_vec(),
        _ => Vec::new(),
    };
    if part_of_page(&sub) {
        return Err(bad("this kind of annotation is part of the page and is not changed here"));
    }
    Ok(Found { r, d, sub, items })
}

/// The flags of an annotation, read the way the renderer reads them ([`Object::as_flags`]).
pub(crate) fn flags_of(doc: &Document, d: &Dict) -> i64 {
    d.get("F").and_then(|f| doc.resolve(f).ok()).map_or(0, |f| f.as_flags())
}

/// Refuse what the annotation's flags do not allow (Table 165): Locked and ReadOnly stop it being moved, resized or drawn again; with `text`
/// (a change of what it says, which Locked does not restrict) ReadOnly and LockedContents stop it.
fn guard(ctx: &Ctx<'_>, f: &Found, text: bool) -> Result<()> {
    let barred = if text { FLAG_READONLY | FLAG_LOCKED_CONTENTS } else { FLAG_READONLY | FLAG_LOCKED };
    if flags_of(ctx.doc, &f.d) & barred != 0 {
        return Err(bad("the annotation is locked: its author does not allow it to be changed"));
    }
    Ok(())
}

/// The numbers of an array entry (the array and its elements may be indirect); `None` if it is not an array of at most `max` numbers.
pub(super) fn numbers(doc: &Document, o: Option<&Object>, max: usize) -> Option<Vec<f64>> {
    let held = doc.resolve(o?).ok()?;
    let items = held.as_array()?;
    if items.len() > max {
        return None;
    }
    items.iter().map(|x| doc.resolve(x).ok().and_then(|v| v.as_f64()).filter(|v| v.is_finite())).collect()
}

/// `/Rect` in user space, `[left, bottom, right, top]` in order.
pub(crate) fn rect_in(doc: &Document, d: &Dict) -> Option<[f64; 4]> {
    let v = numbers(doc, d.get("Rect"), 4)?;
    let [a, b, c, e] = v.as_slice() else { return None };
    Some([a.min(*c), b.min(*e), a.max(*c), b.max(*e)])
}

fn rect_of(doc: &Document, d: &Dict) -> Result<[f64; 4]> {
    rect_in(doc, d).ok_or_else(|| bad("the annotation's rectangle cannot be read"))
}

/// Where an annotation with the user-space rectangle `user` and the flags `flags` is on the page as shown: `[left, top, right, bottom]`. One
/// that is not turned with the page (NoRotate, 12.5.3) hangs from where the upper left corner of its rectangle (in user space) is, upright.
pub(crate) fn shown_rect(shown: &Shown, flags: i64, user: [f64; 4]) -> [f64; 4] {
    if flags & FLAG_NO_ROTATE != 0 {
        let (px, py) = shown.map(user[0], user[3]);
        return [px, py, px + (user[2] - user[0]), py + (user[3] - user[1])];
    }
    let corners = [(user[0], user[1]), (user[0], user[3]), (user[2], user[1]), (user[2], user[3])];
    corners.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, &(x, y)| {
        let (px, py) = shown.map(x, y);
        [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)]
    })
}

/// The user-space rectangle a rectangle of the page as shown becomes.
fn user_rect(shown: &Shown, [l, t, r, b]: [f64; 4]) -> [f64; 4] {
    let (p, q) = (shown.unmap(l, b), shown.unmap(r, t));
    [p.0.min(q.0), p.1.min(q.1), p.0.max(q.0), p.1.max(q.1)]
}

fn date(now: &str) -> Object {
    Object::String(PdfString::literal(now.as_bytes().to_vec()))
}

// --- the points of an annotation ----------------------------------------------------------------------------------------

/// Change the points an annotation has (`/QuadPoints`, `/L`, `/Vertices`, `/CL` and every stroke of `/InkList` and `/Path`) with `f`, charging the
/// work. A point that comes out not finite, or beyond what a PDF holds, refuses the whole change.
fn map_points(ctx: &Ctx<'_>, d: &mut Dict, f: &dyn Fn(f64, f64) -> (f64, f64)) -> Result<()> {
    let mut left = MAX_GEOMETRY;
    let unreadable = || bad("the points of the annotation cannot be read");
    let mut each = |v: &[f64]| -> Result<Vec<f64>> {
        // (A stroke of no points counts and is charged as one: a million empty ones are not free.)
        let n = v.len().max(1);
        left = left.checked_sub(n).ok_or_else(|| too_much("the annotation has too many points"))?;
        if !ctx.work.charge(NUMBER_COST * n as f64) {
            return Err(too_much("the annotation took too much work"));
        }
        let (pairs, rest) = v.as_chunks::<2>();
        if !rest.is_empty() {
            return Err(unreadable());
        }
        let ok = |v: f64| v.is_finite() && v.abs() <= REAL_LIMIT;
        let mut out = Vec::with_capacity(v.len());
        for &[x, y] in pairs {
            let (x, y) = f(x, y);
            if !(ok(x) && ok(y)) {
                return Err(bad("a point of the annotation would be too far from the page"));
            }
            out.extend([x, y]);
        }
        Ok(out)
    };
    for key in ["QuadPoints", "L", "Vertices", "CL"] {
        if d.contains_key(key) {
            let v = numbers(ctx.doc, d.get(key), MAX_GEOMETRY).ok_or_else(unreadable)?;
            let moved = each(&v)?;
            d.set(key, reals(&moved));
        }
    }
    // (`/Path` of PDF 2.0 is strokes too, with curves: every pair of numbers in it is a point.)
    for key in ["InkList", "Path"] {
        let Some(o) = d.get(key) else { continue };
        let held = ctx.doc.resolve(o)?;
        let strokes = held.as_array().ok_or_else(unreadable)?;
        if strokes.len() > MAX_GEOMETRY / 2 {
            return Err(too_much("the annotation has too many strokes"));
        }
        let mut out = Vec::with_capacity(strokes.len());
        for s in strokes {
            let v = numbers(ctx.doc, Some(s), MAX_GEOMETRY).ok_or_else(unreadable)?;
            out.push(reals(&each(&v)?));
        }
        d.set(key, Object::Array(out));
    }
    Ok(())
}

/// The distance a rectangle `at` (`[left, top, right, bottom]` of the page as shown) is moved when it is asked to go `delta` on a page of the size
/// `page`: as asked, but not so far that less than 8 points of it (or all of it, if it is smaller) stay on the page. The window uses it too, for the
/// frame it shows while the annotation is dragged. (Not `clamp`: that panics when the least is more than the most, as it is on a page narrower than
/// the part that is kept.)
pub fn keep_on_page(at: [f64; 4], delta: (f64, f64), page: (f64, f64)) -> (f64, f64) {
    let along = |lo: f64, hi: f64, room: f64, d: f64| {
        let keep = KEEP_ON_PAGE.min(hi - lo);
        d.max(keep - hi).min(room - keep - lo)
    };
    (along(at[0], at[2], page.0, delta.0), along(at[1], at[3], page.1, delta.1))
}

// --- moving --------------------------------------------------------------------------------------------------------------

/// Move the annotation by `delta` points of the page as shown (held so that some of it stays on the page).
pub(super) fn move_by(ctx: &mut Ctx<'_>, pages: &[Page], page: &Page, index: u32, num: u32, delta: [f32; 2], now: &str) -> Result<()> {
    let f = find(ctx, pages, page, index, num)?;
    guard(ctx, &f, false)?;
    let shown = Shown::of(page);
    let user = rect_of(ctx.doc, &f.d)?;
    let at = shown_rect(&shown, flags_of(ctx.doc, &f.d), user);
    let (dx, dy) = keep_on_page(at, (finite_place(delta[0])?, finite_place(delta[1])?), shown.size());
    let (o, n) = (shown.unmap(0.0, 0.0), shown.unmap(dx, dy));
    let (tx, ty) = (n.0 - o.0, n.1 - o.1);
    let by = move |x: f64, y: f64| (x + tx, y + ty);
    // What goes with it moves too: its popup, the replies to it and to those, and their popups (a locked reply stops the move).
    let along = if f.r.num == 0 { IdSet::new() } else { super::companions(ctx.doc, &f.items, f.r.num, &f.d, "a reply to the annotation is locked: it is not moved")? };
    let mut d = f.d.clone();
    put_reals(&mut d, "Rect", &[user[0] + tx, user[1] + ty, user[2] + tx, user[3] + ty]);
    map_points(ctx, &mut d, &by)?;
    d.set("M", date(now));
    for &n in along.iter().filter(|&&n| n != f.r.num) {
        let Ok(Object::Dict(mut o)) = ctx.doc.get(ObjRef::new(n, 0)) else { continue };
        if let Some([x0, y0, x1, y1]) = rect_in(ctx.doc, &o) {
            let (a, b) = (by(x0, y0), by(x1, y1));
            put_reals(&mut o, "Rect", &[a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]);
        }
        map_points(ctx, &mut o, &by)?;
        if !o.get_name("Subtype").is_some_and(|s| s == "Popup") {
            o.set("M", date(now));
        }
        ctx.objects.push((n, Object::Dict(o)));
    }
    ctx.objects.push((f.r.num, Object::Dict(d)));
    ctx.tally(0)
}

// --- what an annotation looks like now -----------------------------------------------------------------------------------

/// A colour entry (`/C`, `/IC`) as RGB: gray, RGB or CMYK.
fn rgb_of(doc: &Document, d: &Dict, key: &str) -> Option<[u8; 3]> {
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match numbers(doc, d.get(key), 4).as_deref() {
        Some([g]) => Some([to8(*g); 3]),
        Some([r, g, b]) => Some([to8(*r), to8(*g), to8(*b)]),
        Some([c, m, y, k]) => Some([to8((1.0 - c) * (1.0 - k)), to8((1.0 - m) * (1.0 - k)), to8((1.0 - y) * (1.0 - k))]),
        _ => None,
    }
}

/// The width of the border as the file says it: `/BS /W`, else the third number of `/Border`, else one point (0 is no border).
fn raw_width(doc: &Document, d: &Dict) -> f64 {
    let from_bs = d.get("BS").and_then(|b| doc.resolve(b).ok()).and_then(|b| b.as_dict().and_then(|b| b.get("W")).and_then(|w| doc.resolve(w).ok()).and_then(|w| w.as_f64()));
    let from_border = || numbers(doc, d.get("Border"), 4).and_then(|b| b.get(2).copied());
    from_bs.or_else(from_border).unwrap_or(1.0)
}

/// The width of the pen, within the limits of a drawing.
fn width_of(doc: &Document, d: &Dict) -> f64 {
    shapes::clamp_width(raw_width(doc, d))
}

/// Has the annotation more text than the reader changes (a note or a text box keeps at most [`MAX_NOTE_CHARS`] characters, as many as
/// `MAX_FREETEXT_CHARS`)? Changing it would cut it.
fn too_long(doc: &Document, d: &Dict) -> bool {
    contents_of(doc, d, MAX_NOTE_CHARS + 1).chars().count() > MAX_NOTE_CHARS
}

/// The full `/Contents` of an annotation (at most `limit` characters), line ends as `\n`.
pub(crate) fn contents_of(doc: &Document, d: &Dict, limit: usize) -> String {
    match d.get("Contents").and_then(|c| doc.resolve(c).ok()) {
        Some(Object::String(s)) => decode_text_lines(&s.bytes).replace("\r\n", "\n").replace('\r', "\n").chars().take(limit).collect(),
        _ => String::new(),
    }
}

/// The points of the strokes of `/InkList`.
fn strokes_of(ctx: &Ctx<'_>, d: &Dict) -> Result<Vec<Vec<(f64, f64)>>> {
    let unreadable = || bad("the points of the annotation cannot be read");
    let held = ctx.doc.resolve(d.get("InkList").ok_or_else(unreadable)?)?;
    let strokes = held.as_array().ok_or_else(unreadable)?;
    if strokes.len() > MAX_GEOMETRY / 2 {
        return Err(too_much("the drawing has too many strokes"));
    }
    let mut total = 0usize;
    let mut out = Vec::new();
    for s in strokes {
        let v = numbers(ctx.doc, Some(s), MAX_GEOMETRY).ok_or_else(unreadable)?;
        let (pairs, rest) = v.as_chunks::<2>();
        total += pairs.len().max(1);
        if total > MAX_GEOMETRY / 2 || !rest.is_empty() {
            return Err(too_much("the drawing has too many points"));
        }
        out.push(pairs.iter().map(|&[x, y]| (x, y)).collect());
    }
    if !ctx.work.charge(NUMBER_COST * total as f64 * 2.0) {
        return Err(too_much("the drawing took too much work"));
    }
    Ok(out)
}

/// The lines of a text markup as quadrilaterals in the order the reader draws them in (upper left, upper right, lower left, lower right as the
/// page is shown), whatever order the file keeps the corners in: each is the box that holds its four corners.
fn quads_of(ctx: &Ctx<'_>, shown: &Shown, d: &Dict) -> Result<Vec<Quad>> {
    let unreadable = || bad("the quadrilaterals of the markup cannot be read");
    let v = numbers(ctx.doc, d.get("QuadPoints"), MAX_QUADS * 8).ok_or_else(unreadable)?;
    let (quads, rest) = v.as_chunks::<8>();
    if quads.is_empty() || !rest.is_empty() {
        return Err(unreadable());
    }
    Ok(quads
        .iter()
        .map(|q| {
            let b = q.as_chunks::<2>().0.iter().map(|&[x, y]| shown.map(x, y)).fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, (x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]);
            [(b[0], b[1]), (b[2], b[1]), (b[0], b[3]), (b[2], b[3])].map(|(x, y)| shown.unmap(x, y))
        })
        .collect())
}

fn markup_kind(sub: &[u8]) -> Option<MarkupKind> {
    match sub {
        b"Highlight" => Some(MarkupKind::Highlight),
        b"Underline" => Some(MarkupKind::Underline),
        b"StrikeOut" => Some(MarkupKind::StrikeOut),
        b"Squiggly" => Some(MarkupKind::Squiggly),
        _ => None,
    }
}

/// The pen of a drawing: colour and width.
fn set_pen(d: &mut Dict, rgb: [u8; 3], width: f64) {
    put_reals(d, "C", &unit(rgb));
    let mut border = Dict::new();
    border.set("W", Object::Real(width));
    border.set("S", Object::from("S"));
    d.set("BS", Object::Dict(border));
}

// --- drawing an annotation again -----------------------------------------------------------------------------------------

/// What a redraw changes of the look: the colour, the width of the pen and the fill (squares and circles); `None` keeps what the annotation has.
#[derive(Default)]
struct Pen {
    color: Option<[u8; 3]>,
    width: Option<f64>,
    fill: Option<bool>,
}

/// Does the annotation have something that drawing it again would lose: a dash pattern, the rich text or style of a text box, its border, fill or
/// callout or an opacity it is drawn with, a cloud, a leader line, a caption, a curve of a drawing, a note icon the reader does not draw, a
/// square or circle without a border? `turned`: the page is turned, which a text box that stays upright cannot be drawn again on.
fn lossy(doc: &Document, d: &Dict, sub: &[u8], turned: bool) -> bool {
    let at = |k: &str| d.get(k).and_then(|v| doc.resolve(v).ok());
    let number = |k: &str| at(k).and_then(|v| v.as_f64());
    let dash = at("BS").as_ref().and_then(Object::as_dict).is_some_and(|b| b.contains_key("D") || b.get_name("S").is_some_and(|s| s == "D"));
    let dotted = at("Border").as_ref().and_then(Object::as_array).and_then(|a| a.get(3)).and_then(|x| doc.resolve(x).ok()).is_some_and(|x| x.as_array().is_some_and(|a| !a.is_empty()));
    dash || dotted
        || match sub {
            b"Square" | b"Circle" => raw_width(doc, d) == 0.0 || at("BE").as_ref().and_then(Object::as_dict).is_some_and(|b| b.get_name("S").is_some_and(|s| s == "C")),
            b"Line" => number("LL").is_some_and(|l| l != 0.0) || at("Cap") == Some(Object::Bool(true)),
            b"Ink" => d.contains_key("Path"),
            b"Text" => at("Name").is_some_and(|n| !matches!(&n, Object::Name(n) if n == "Comment")),
            b"FreeText" => {
                ["CL", "RC", "DS", "C", "IC", "BE"].iter().any(|k| d.contains_key(k))
                    || number("Q").is_some_and(|q| q != 0.0)
                    || number("CA").is_some_and(|c| c < 1.0)
                    || raw_width(doc, d) > 0.0
                    || (turned && flags_of(doc, d) & FLAG_NO_ROTATE != 0)
                    || too_long(doc, d)
            }
            _ => false,
        }
}

/// Draw the annotation `d` (a `sub`) again from what it holds, the kinds the reader draws: text markup, drawings, squares, circles, lines, notes
/// and text boxes. `pen` has the colour, the width of the line and the fill that replace the old. `d` gets the entries that go with the new
/// appearance (`/Rect` where the drawing decides it); the new form is returned. What the annotation says of its opacity (`/CA`) is drawn with;
/// what would be lost ([`lossy`]) is not drawn again. A text box is drawn again in `fit` its rectangle.
fn redraw(ctx: &mut Ctx<'_>, shown: &Shown, sub: &[u8], d: &mut Dict, pen: Pen, fit: Fit) -> Result<ObjRef> {
    if !ctx.work.charge(REDRAW_COST) {
        return Err(too_much("the annotation took too much work"));
    }
    let doc = ctx.doc;
    if lossy(doc, d, sub, shown.is_turned()) {
        return Err(bad(CANNOT_AGAIN));
    }
    let Pen { color, width, fill } = pen;
    let ca = d.get("CA").and_then(|c| doc.resolve(c).ok()).and_then(|c| c.as_f64()).filter(|c| *c < 1.0).map(|c| c.max(0.0));
    let rgb = color.or_else(|| rgb_of(doc, d, "C")).unwrap_or([0, 0, 0]);
    let w = width.map_or_else(|| width_of(doc, d), shapes::clamp_width);
    let unreadable = || bad("the ends of the line cannot be read");
    let ap = match sub {
        s if markup_kind(s).is_some() => {
            let ap = appearance::markup(markup_kind(s).unwrap_or(MarkupKind::Highlight), &quads_of(ctx, shown, d)?, unit(rgb));
            put_reals(d, "Rect", &ap.bbox);
            put_reals(d, "C", &unit(rgb));
            ap
        }
        b"Ink" => {
            let ap = appearance::ink(&strokes_of(ctx, d)?, unit(rgb), w);
            set_pen(d, rgb, w);
            put_reals(d, "Rect", &ap.bbox);
            ap
        }
        b"Square" | b"Circle" => {
            // The shape is what `/RD` leaves of `/Rect` (12.5.6.8; none: the whole of it, less half the old pen); the new pen is centred on its edge.
            let rect = rect_of(doc, d)?;
            let old = width_of(doc, d) / 2.0;
            let inner = match numbers(doc, d.get("RD"), 4).as_deref() {
                Some([l, t, r, b]) => [rect[0] + l, rect[1] + b, rect[2] - r, rect[3] - t],
                _ => [rect[0] + old, rect[1] + old, rect[2] - old, rect[3] - old],
            };
            if inner[2] - inner[0] <= 1.0 || inner[3] - inner[1] <= 1.0 {
                return Err(bad("the shape is too small for a line that wide"));
            }
            let rect = [inner[0] - w / 2.0, inner[1] - w / 2.0, inner[2] + w / 2.0, inner[3] + w / 2.0];
            put_reals(d, "Rect", &rect);
            // (A square keeps the colour of its fill unless it is recoloured, when the fill follows: the colour lightened.)
            let fill_rgb = match (fill, color) {
                (Some(true), _) => Some(shapes::lighten(rgb)),
                (Some(false), _) => None,
                (None, Some(_)) if d.contains_key("IC") => Some(shapes::lighten(rgb)),
                (None, _) => rgb_of(doc, d, "IC"),
            };
            let look = Look { rgb, width: w, fill: fill_rgb };
            shapes::set_look(d, &look);
            put_reals(d, "RD", &[w / 2.0; 4]);
            shapes::boxed(sub == b"Circle", rect, &look)
        }
        b"Line" => {
            let [x0, y0, x1, y1] = numbers(doc, d.get("L"), 4).and_then(|l| <[f64; 4]>::try_from(l).ok()).ok_or_else(unreadable)?;
            let held = d.get("LE").and_then(|e| doc.resolve(e).ok());
            let name = |i: usize| match held.as_ref().and_then(Object::as_array).and_then(|a| a.get(i)).and_then(|n| doc.resolve(n).ok()) {
                Some(Object::Name(n)) => shapes::end_of(n.as_bytes()),
                _ => Some(End::Plain),
            };
            let (Some(first), Some(last)) = (name(0), name(1)) else { return Err(bad(CANNOT_AGAIN)) };
            // (The interior colour, `/IC`, is the fill of a closed head: it is kept and used.)
            let look = Look { rgb, width: w, fill: rgb_of(doc, d, "IC") };
            let ap = shapes::line((x0, y0), (x1, y1), [first, last], &look);
            let interior = d.get("IC").cloned();
            shapes::set_look(d, &look);
            if let Some(ic) = interior {
                d.set("IC", ic);
            }
            put_reals(d, "Rect", &ap.bbox);
            ap
        }
        b"Text" => {
            put_reals(d, "C", &unit(rgb));
            // (The icon is the speech bubble, `Comment`: a note that has no name is given it.)
            if !d.contains_key("Name") {
                d.set("Name", Object::from("Comment"));
            }
            appearance::note(unit(rgb))
        }
        b"FreeText" => {
            let at = shown_rect(shown, flags_of(doc, d), rect_of(doc, d)?);
            // A box that may grow must be on the page already: one that hangs over its edge would be moved, and that is not what was asked.
            if fit == Fit::Grow && boxes::normalized(shown, at.map(|v| v as f32))?.iter().zip(&at).take(3).any(|(a, b)| (a - b).abs() > 0.05) {
                return Err(bad("the text box hangs over the edge of the page: put it on the page first"));
            }
            let (size, old) = match d.get("DA").and_then(|c| doc.resolve(c).ok()) {
                Some(Object::String(s)) => {
                    let da = crate::layout::parse_da(&s.bytes);
                    (boxes::clamp_size(if da.size > 0.0 { da.size } else { 12.0 })?, da.color)
                }
                _ => (12.0, "0 g".to_string()),
            };
            let clean = boxes::clean_text(&contents_of(doc, d, super::MAX_FREETEXT_CHARS));
            if clean.trim().is_empty() {
                return Err(bad("the text box is empty"));
            }
            let da = color.map_or(old, boxes::da_color);
            let (form, user) = boxes::freetext_ap(ctx, shown, at, &clean, size, &da, fit)?;
            if fit == Fit::Grow {
                put_reals(d, "Rect", &user);
            }
            d.set("DA", boxes::da_string(size, &da));
            d.remove("RC");
            return Ok(form);
        }
        _ => return Err(bad("the reader does not draw this kind of annotation again")),
    };
    ctx.form_with(&ap, ca)
}

/// Draw the annotation again in the colour `color` and with the line `width` points wide, for the kinds the reader draws; `fill` says
/// whether a square or a circle is filled (lightened colour). `None` leaves the old. Other kinds are refused.
#[allow(clippy::too_many_arguments)]
pub(super) fn restyle(ctx: &mut Ctx<'_>, pages: &[Page], page: &Page, index: u32, num: u32, color: Option<[u8; 3]>, width: Option<f32>, fill: Option<bool>, now: &str) -> Result<()> {
    let f = find(ctx, pages, page, index, num)?;
    guard(ctx, &f, false)?;
    let mut d = f.d.clone();
    let form = redraw(ctx, &Shown::of(page), &f.sub, &mut d, Pen { color, width: width.map(f64::from), fill }, Fit::Fixed)?;
    finish(ctx, f.r, d, Some(form), now)
}

/// Put the changed annotation, with a new appearance if there is one, among the objects of the edit.
fn finish(ctx: &mut Ctx<'_>, r: ObjRef, mut d: Dict, form: Option<ObjRef>, now: &str) -> Result<()> {
    d.set("M", date(now));
    if let Some(form) = form {
        with_appearance(&mut d, form);
    }
    ctx.objects.push((r.num, Object::Dict(d)));
    ctx.tally(0)
}

// --- resizing ------------------------------------------------------------------------------------------------------------

/// Put the annotation in the rectangle `rect` (`[left, top, right, bottom]` of the page as shown). What the reader draws (squares, circles, lines,
/// drawings, text boxes) is drawn again in it; a picture, a polygon keeps its appearance, which Algorithm 8.1 stretches over the new `/Rect`.
pub(super) fn resize(ctx: &mut Ctx<'_>, pages: &[Page], page: &Page, index: u32, num: u32, rect: [f32; 4], now: &str) -> Result<()> {
    let f = find(ctx, pages, page, index, num)?;
    guard(ctx, &f, false)?;
    let doc = ctx.doc;
    let flags = flags_of(doc, &f.d);
    if flags & FLAG_NO_ROTATE != 0 || !matches!(f.sub.as_slice(), b"Square" | b"Circle" | b"Line" | b"Ink" | b"FreeText" | b"Stamp" | b"Polygon" | b"PolyLine") {
        return Err(bad("this kind of annotation has a size that is not changed"));
    }
    let shown = Shown::of(page);
    let user = rect_of(doc, &f.d)?;
    let old = shown_rect(&shown, flags, user);
    let new = boxes::normalized(&shown, rect)?;
    // Where a point of the old rectangle goes in the new one: the same place in proportion, found on the page as shown.
    let (ow, oh) = (old[2] - old[0], old[3] - old[1]);
    if ow < 1.0 || oh < 1.0 {
        return Err(bad("the annotation is too small to be resized"));
    }
    let (sx, sy) = ((new[2] - new[0]) / ow, (new[3] - new[1]) / oh);
    let scale = |x: f64, y: f64| {
        let (px, py) = shown.map(x, y);
        shown.unmap(new[0] + (px - old[0]) * sx, new[1] + (py - old[1]) * sy)
    };
    let mut d = f.d.clone();
    put_reals(&mut d, "Rect", &user_rect(&shown, new));
    map_points(ctx, &mut d, &scale)?;
    let form = if f.sub == b"Stamp" || f.sub.starts_with(b"Poly") { None } else { Some(redraw(ctx, &shown, &f.sub, &mut d, Pen::default(), Fit::Grow)?) };
    finish(ctx, f.r, d, form, now)
}

// --- the text -------------------------------------------------------------------------------------------------------------

/// Give the annotation the text `text` (its `/Contents`); a text box is drawn again with it.
pub(super) fn set_text(ctx: &mut Ctx<'_>, pages: &[Page], page: &Page, index: u32, num: u32, text: &str, now: &str) -> Result<()> {
    let f = find(ctx, pages, page, index, num)?;
    guard(ctx, &f, true)?;
    // (A text that is longer than the reader keeps would be cut.)
    if too_long(ctx.doc, &f.d) {
        return Err(bad("the text is too long to be changed here"));
    }
    let mut d = f.d.clone();
    let form = if f.sub == b"FreeText" {
        let clean = boxes::clean_text(text);
        d.set("Contents", text_string(&clean.replace('\n', "\r")));
        // (The box of a locked annotation is not changed: the text has to fit in it.)
        let fit = if flags_of(ctx.doc, &d) & FLAG_LOCKED != 0 { Fit::Fixed } else { Fit::Grow };
        Some(redraw(ctx, &Shown::of(page), &f.sub, &mut d, Pen::default(), fit)?)
    } else {
        d.remove("RC");
        d.set("Contents", text_string(&text.chars().take(MAX_NOTE_CHARS).collect::<String>()));
        None
    };
    finish(ctx, f.r, d, form, now)
}
