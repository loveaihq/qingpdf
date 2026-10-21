//! Redaction of one page (2026-10-20; ISO 32000-2 12.5.6.23 for the marks, 9.4 and 8.9 for what is taken out). The page is read once as for
//! editing its text (`text::EditScanner`, here also reading the text of artifacts, and noting the pictures and paths drawn), and what lies under a
//! mark is taken out of the page itself:
//!
//! - **text**: every glyph whose box meets a mark (invisible text, the text of forms, Type 3 glyphs too: a glyph is a glyph) is cut out of its string,
//!   and a number of a `TJ` takes the place of the width it had, so that what follows stays where it was. A marked-content sequence with an
//!   `/ActualText` that holds a glyph that goes loses the entry.
//! - **pictures**: the pixels under a mark are painted over in the picture and the picture is coded again (`pixels.rs`); a picture drawn in
//!   more than one place is copied first, one copy for each place that is touched, and the page draws the copy. Inline pictures become
//!   picture objects. A picture that cannot be read or coded again is left out of the page.
//! - **paths**: a path (a line, a rectangle, a shape) whose bounds are wholly inside a mark is not painted. A path that only partly lies under a mark
//!   stays whole: it is shown cut by the box; this is said in the notes of the feature.
//! - **annotations**: those whose rectangle meets a mark go, with a popup that belongs to a removed one (links too, and form widgets, which also go
//!   from the form: `edit::redact`).
//! - then the boxes are drawn into the page's content, and the marks (the `Redact` annotations) are taken off.
//!
//! The forms (the same ones that 5a copies) are copied when they change, one copy for every page that has them changed; nothing of the old
//! page is kept in the new objects. A change that cannot be written (a statement cut across two streams, a stream the page lists twice) stops
//! the whole redaction with an error: a page is not given out half done.

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::pixels::{self, Outcome};
use super::{Frag, PageEdits, Run, effective_resources, resolve_dict};
use crate::document::{Document, Page};
use crate::edit::content::{MAX_BALANCE, cm, scan_balance, upright};
use crate::edit::fontsub::{self, Look};
use crate::edit::Ctx;
use crate::error::{Error, Result};
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::render::export::inline_image;
use crate::render::work::Work;
use crate::text::{DrawKind, EditScanner, Glyph, Scan, Shown};

/// Most rectangles of marks one page may have.
pub const MAX_RECTS: usize = 20_000;
/// The work meter's charge for a glyph looked at and a rectangle looked at against it.
const GLYPH_COST: f64 = 40.0;
const TEST_COST: f64 = 6.0;
/// A box is taken to meet a mark when they overlap by more than this (points).
const EPS: f64 = 1.0e-3;
/// A path is inside a mark when its bounds are, to within this (points).
const TOLERANCE: f64 = 0.25;
/// Size limits of the label on a box: points high at the most and at the least.
const LABEL_MAX: f64 = 14.0;
const LABEL_MIN: f64 = 4.0;

/// A mark: the rectangles (`[left, top, right, bottom]`, points of the page as shown) that are to be taken out and covered, the colour of the
/// box and the words on it (empty: none).
#[derive(Clone, Debug)]
pub(in crate::edit) struct Mark {
    pub rects: Vec<[f32; 4]>,
    pub color: [u8; 3],
    pub label: String,
}

/// What was taken off a page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub glyphs: usize,
    pub images: usize,
    pub paths: usize,
    pub annots: usize,
}

/// A mark's rectangle on the page as shown and in default user space, and the mark it is of.
struct Zone {
    shown: [f64; 4],
    user: [f64; 4],
    mark: usize,
}

fn meets(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] - EPS && b[0] < a[2] - EPS && a[1] < b[3] - EPS && b[1] < a[3] - EPS
}

fn within(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] >= b[0] - TOLERANCE && a[1] >= b[1] - TOLERANCE && a[2] <= b[2] + TOLERANCE && a[3] <= b[3] + TOLERANCE
}

pub(in crate::edit) fn bounds_of(points: impl IntoIterator<Item = (f64, f64)>) -> [f64; 4] {
    points.into_iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, (x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)])
}

/// The rectangles of the marks, as zones.
fn zones(shown: &Shown, marks: &[Mark]) -> Result<Vec<Zone>> {
    let mut out = Vec::new();
    for (n, m) in marks.iter().enumerate() {
        for r in &m.rects {
            let r = r.map(f64::from);
            if !r.iter().all(|v| v.is_finite()) {
                continue;
            }
            let b = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];
            if b[2] - b[0] < 0.01 || b[3] - b[1] < 0.01 {
                continue;
            }
            let user = bounds_of([(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])].map(|(x, y)| shown.unmap(x, y)));
            out.push(Zone { shown: b, user, mark: n });
        }
    }
    if out.len() > MAX_RECTS {
        return Err(Error::Limit(format!("a page has at most {MAX_RECTS} rectangles to redact")));
    }
    Ok(out)
}

/// The box that a glyph is taken to cover: the cell it is drawn in, a little in from its ends along the line (a mark made of the cells of
/// the characters round it leaves it alone), from the baseline to seven tenths of the size above it (the lines of a page set tightly have
/// cells that overlap by the descenders and the ascenders). A vertical glyph is centred on its column.
fn ink_box(shown: &Shown, g: &Glyph) -> [f64; 4] {
    let (a0, a1) = if g.vertical { (-0.4 * g.size, 0.4 * g.size) } else { (0.0, 0.7 * g.size) };
    let (nx, ny) = (-g.dy, g.dx);
    let mut points = Vec::with_capacity(4);
    for along in [g.adv * 0.1, g.adv * 0.9] {
        for across in [a0, a1] {
            points.push(shown.map(g.x + along * g.dx + across * nx, g.y + along * g.dy + across * ny));
        }
    }
    bounds_of(points)
}

/// The text of the page after the cut: the glyphs that go.
fn glyphs_cut(scan: &Scan, shown: &Shown, zones: &[Zone], work: &Work) -> Result<Vec<bool>> {
    let all = bounds_of(zones.iter().flat_map(|z| [(z.shown[0], z.shown[1]), (z.shown[2], z.shown[3])]));
    if !work.charge(GLYPH_COST * scan.glyphs.len() as f64) {
        return Err(Error::Limit("redacting the page took too much work".to_string()));
    }
    let mut tests = 0usize;
    let mut cut: Vec<bool> = scan
        .glyphs
        .iter()
        .map(|g| {
            let b = ink_box(shown, g);
            if !b.iter().all(|v| v.is_finite()) || !meets(&b, &all) {
                return false;
            }
            tests += zones.len();
            zones.iter().any(|z| meets(&b, &z.shown))
        })
        .collect();
    if !work.charge(TEST_COST * tests as f64) {
        return Err(Error::Limit("redacting the page took too much work".to_string()));
    }
    guessed_lines(scan, shown, zones, &mut cut, work)?;
    Ok(cut)
}

/// A font that has no widths puts its glyphs where a guess says, not where they are. The glyphs of an operator that shows with one all go when a mark
/// crosses the line of the operator: how high the line is (the size of the font, where it stands) is known, how long it is, and so where the text is along
/// it, is not.
fn guessed_lines(scan: &Scan, shown: &Shown, zones: &[Zone], cut: &mut [bool], work: &Work) -> Result<()> {
    let mut g = 0usize;
    while g < scan.glyphs.len() {
        let show = show_of(scan, g);
        let mut last = g;
        while show.is_some() && last + 1 < scan.glyphs.len() && show_of(scan, last + 1) == show {
            last += 1;
        }
        let lo = g;
        g = last + 1;
        let (Some(first), Some(font)) = (scan.glyphs.get(lo), show.and_then(|s| scan.shows.get(s as usize)).and_then(|s| scan.fonts.get(s.font as usize))) else { continue };
        if !font.font.guessed_widths() {
            continue;
        }
        if !work.charge(TEST_COST * zones.len() as f64) {
            return Err(Error::Limit("redacting the page took too much work".to_string()));
        }
        // The line's width in the page as shown: the normal of its direction, and where the two sides of the line stand along it.
        let at = |across: f64| shown.map(first.x - across * first.dy, first.y + across * first.dx);
        let (o, e) = (at(0.0), shown.map(first.x + first.dx, first.y + first.dy));
        let len = (e.0 - o.0).hypot(e.1 - o.1);
        if !(len.is_finite() && len > 0.0) {
            continue;
        }
        let n = (-(e.1 - o.1) / len, (e.0 - o.0) / len);
        let side = |p: (f64, f64)| p.0 * n.0 + p.1 * n.1;
        let (a0, a1) = if first.vertical { (-0.4 * first.size, 0.4 * first.size) } else { (0.0, 0.7 * first.size) };
        let (s0, s1) = (side(at(a0)), side(at(a1)));
        let (lo_s, hi_s) = (s0.min(s1), s0.max(s1));
        let crossed = zones.iter().any(|z| {
            let c = [(z.shown[0], z.shown[1]), (z.shown[2], z.shown[1]), (z.shown[2], z.shown[3]), (z.shown[0], z.shown[3])].map(side);
            let (zl, zh) = (c.iter().copied().fold(f64::INFINITY, f64::min), c.iter().copied().fold(f64::NEG_INFINITY, f64::max));
            lo_s < zh - EPS && zl < hi_s - EPS
        });
        if crossed {
            for slot in cut.get_mut(lo..=last).unwrap_or(&mut []) {
                *slot = true;
            }
        }
    }
    Ok(())
}

fn show_of(scan: &Scan, glyph: usize) -> Option<u32> {
    scan.chars.get(glyph).and_then(|r| scan.parts.get(r.part as usize)).map(|p| p.show)
}

/// The number of a `TJ` that stands where the glyphs `lo..=hi` of one operator were, so that what follows stays where it was.
fn gap_number(scan: &Scan, show: u32, lo: usize, hi: usize) -> Option<f64> {
    let show = scan.shows.get(show as usize)?;
    let (a, b) = (scan.glyphs.get(lo)?, scan.glyphs.get(hi)?);
    let along = (b.x + b.adv * b.dx - a.x) * a.dx + (b.y + b.adv * b.dy - a.y) * a.dy;
    let per = if a.vertical { show.size } else { show.size * show.th };
    if show.scale <= 0.0 || per == 0.0 {
        return None;
    }
    // (Horizontal: the text moves on by `-n / 1000 * size * Th` for a number `n`; vertical, down by `-n / 1000 * size`; 9.4.3.)
    let n = if a.vertical { along / show.scale * 1000.0 / per } else { -along / show.scale * 1000.0 / per };
    (n.is_finite() && n.abs() > 1.0e-3).then_some(n)
}

/// Cut the glyphs `cut` out of the page's text: for each run of glyphs of one operator that go one after the other, the characters are taken
/// out of their strings and a number takes their place. The number of glyphs cut.
fn plan_text(scan: &Scan, cut: &[bool], edits: &mut PageEdits) -> usize {
    let mut seen: Vec<(u32, u32)> = Vec::new();
    let (mut count, mut g) = (0usize, 0usize);
    while g < cut.len() {
        if cut.get(g) != Some(&true) {
            g += 1;
            continue;
        }
        let show = show_of(scan, g);
        let mut hi = g;
        while cut.get(hi + 1) == Some(&true) && show_of(scan, hi + 1) == show {
            hi += 1;
        }
        let (lo, last) = (g, hi);
        g = hi + 1;
        let (Some(show), Some(first)) = (show, scan.chars.get(lo)) else { continue };
        // (A form drawn more than once shows the same string again: its glyphs are cut once.)
        let at = seen.partition_point(|&p| p < (first.part, first.off));
        if seen.get(at) == Some(&(first.part, first.off)) {
            continue;
        }
        seen.insert(at, (first.part, first.off));
        let id = u32::try_from(edits.hits).unwrap_or(u32::MAX);
        edits.hits += 1;
        for r in (lo..=last).filter_map(|k| scan.chars.get(k)) {
            edits.deleted.push((r.part, r.off, r.len, id));
            count += 1;
        }
        edits.shows.push(show);
        if let Some(n) = gap_number(scan, show, lo, last) {
            edits.inserted.push((first.part, first.off, vec![Frag::Gap(n)]));
            edits.bytes = edits.bytes.saturating_add(64);
        }
        // The marked-content sequences with an `/ActualText` that hold one of the glyphs lose the entry.
        let mut previous = None;
        for k in lo..=last {
            let here = scan.span_at(k);
            if here == previous {
                continue;
            }
            previous = here;
            let mut at = here;
            while let Some(i) = at {
                edits.spans.push((u32::try_from(i).unwrap_or(u32::MAX), None));
                at = scan.spans.get(i).map(|s| s.parent as usize).filter(|&p| p < scan.spans.len());
            }
        }
    }
    count
}

/// The rectangle of an annotation (`/Rect`) in default user space.
pub(in crate::edit) fn rect_of(doc: &Document, d: &Dict) -> Option<[f64; 4]> {
    let Object::Array(a) = doc.resolve(d.get("Rect")?).ok()? else { return None };
    let n: Vec<f64> = a.iter().filter_map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64())).collect();
    let [x0, y0, x1, y1] = n.as_slice() else { return None };
    let b = [x0.min(*x1), y0.min(*y1), x0.max(*x1), y0.max(*y1)];
    b.iter().all(|v| v.is_finite()).then_some(b)
}

/// A name for a picture that the resources of `src` do not have.
fn free_name(doc: &Document, page: &Page, scan: &Scan, src: u32, taken: &[(u32, Vec<u8>, ObjRef)], num: u32) -> Vec<u8> {
    let res = effective_resources(doc, page, scan, src as usize);
    let have = |name: &[u8]| -> bool {
        taken.iter().any(|t| t.0 == src && t.1 == name)
            || res.get("XObject").and_then(|x| doc.resolve(x).ok()).is_some_and(|x| x.as_dict().is_some_and(|d| d.iter().any(|(k, _)| k.as_bytes() == name)))
    };
    let mut name = format!("Qx{num}").into_bytes();
    while have(&name) {
        name.push(b'x');
    }
    name
}

/// The encoding of a string of PDF content for a name (`/Name`).
fn xobject_name(name: &[u8]) -> String {
    format!("/{} Do", String::from_utf8_lossy(name))
}

/// What a label is drawn with: for each character its advance in ems and its outline as the operators of a path (in points at size 1).
struct Label {
    glyphs: Vec<(char, f64, String)>,
}

impl Label {
    fn width(&self, text: &str, size: f64) -> f64 {
        text.chars().filter_map(|c| self.glyphs.iter().find(|g| g.0 == c)).map(|g| g.1 * size).sum()
    }
}

/// The glyph outlines of `text` from a system font, at 1 point high. `None` when no installed font has all the characters.
fn make_label(text: &str) -> Option<Label> {
    let chars = crate::order::set_of(text.chars().filter(|c| !c.is_control()));
    let font = fontsub::choose(&chars, Look::Sans).ok()?;
    let cmap = font.cmap.as_ref()?;
    let e = font.em_matrix();
    let mut glyphs = Vec::new();
    for c in chars {
        let gid = cmap.unicode(c as u32).filter(|&g| g != 0)?;
        // (The advance is in ems already, as `fontsub::add_type3` takes it.)
        let adv_em = f64::from(font.advance(gid)?);
        let (proc_text, _) = crate::render::outline::charproc(&font, gid, [e[0], e[1], e[2], e[3], 0.0, 0.0], adv_em)?;
        // The first line is the width and box of the glyph (`d1`); what is left is the path and its fill.
        let body = proc_text.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string());
        glyphs.push((c, adv_em, body));
    }
    Some(Label { glyphs })
}

/// The drawing of a label centred in the rectangle `r` (`[left, top, right, bottom]`, the page as shown), in the page as shown with y up from the
/// bottom (`height` is the page's): none if it does not fit.
fn draw_label(out: &mut String, label: &Label, text: &str, r: &[f64; 4], height: f64, light_on_dark: bool) {
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    let mut size = (h * 0.6).min(LABEL_MAX);
    let natural = label.width(text, size);
    if natural <= 0.0 {
        return;
    }
    if natural > w * 0.94 {
        size *= w * 0.94 / natural;
    }
    if size < LABEL_MIN {
        return;
    }
    let width = label.width(text, size);
    let (mut x, y) = (r[0] + (w - width) / 2.0, height - r[3] + h / 2.0 - 0.3 * size);
    out.push_str(if light_on_dark { "1 g\n" } else { "0 g\n" });
    for c in text.chars() {
        let Some((_, adv, body)) = label.glyphs.iter().find(|g| g.0 == c) else { continue };
        // The outline is at 1 point high; the matrix scales it to the size.
        if !body.is_empty() {
            out.push_str(&format!("q {size:.3} 0 0 {size:.3} {x:.3} {y:.3} cm\n{body}Q\n"));
        }
        x += adv * size;
    }
}

/// The marks' boxes and labels as the content that draws them over the page.
fn overlay_content(shown: &Shown, marks: &[Mark], labels: &mut Vec<(String, Option<Label>)>, notes: &mut Vec<String>) -> String {
    let (_, height) = shown.size();
    let mut out = String::from("q\n");
    out.push_str(&cm(upright(shown)));
    out.push('\n');
    for m in marks {
        let rects: Vec<[f64; 4]> = m.rects.iter().map(|r| r.map(f64::from)).filter(|r| r.iter().all(|v| v.is_finite())).map(|r| [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]).collect();
        out.push_str(&format!("{:.4} {:.4} {:.4} rg\n", f64::from(m.color[0]) / 255.0, f64::from(m.color[1]) / 255.0, f64::from(m.color[2]) / 255.0));
        for r in &rects {
            out.push_str(&format!("{:.3} {:.3} {:.3} {:.3} re f\n", r[0], height - r[3], r[2] - r[0], r[3] - r[1]));
        }
        if m.label.is_empty() {
            continue;
        }
        let at = match labels.iter().position(|l| l.0 == m.label) {
            Some(i) => i,
            None => {
                let made = make_label(&m.label);
                if made.is_none() {
                    notes.push("no installed font has the characters of the words on the boxes; they are left out".to_string());
                }
                labels.push((m.label.clone(), made));
                labels.len() - 1
            }
        };
        let widest = rects.iter().copied().max_by(|a, b| (a[2] - a[0]).total_cmp(&(b[2] - b[0])));
        if let (Some(r), Some((_, Some(label)))) = (widest, labels.get(at)) {
            let luma = 0.299 * f64::from(m.color[0]) + 0.587 * f64::from(m.color[1]) + 0.114 * f64::from(m.color[2]);
            draw_label(&mut out, label, &m.label, &r, height, luma < 140.0);
        }
    }
    out.push_str("Q\n");
    out
}

/// What a page keeps of a stream that is `bytes`: a stream object (Flate-coded when that is shorter).
fn new_stream(ctx: &mut Ctx<'_>, bytes: Vec<u8>) -> Result<ObjRef> {
    let mut dict = Dict::new();
    let data = if bytes.len() > 200 {
        dict.set("Filter", Object::from("FlateDecode"));
        compress_to_vec_zlib(&bytes, 6)
    } else {
        bytes
    };
    ctx.spend(data.len() + 96)?;
    let r = ctx.alloc()?;
    ctx.objects.push((r.num, Object::Stream(Stream { dict, data })));
    Ok(r)
}

/// Reads and redacts pages. One of these does all the pages of a run: the fonts and forms the reading finds are kept between pages.
pub(in crate::edit) struct Redactor<'a> {
    run: Run<'a, false>,
    labels: Vec<(String, Option<Label>)>,
    /// The form widgets taken off pages: the form is cut down to the fields that are left (`edit::redact`).
    pub widgets: Vec<ObjRef>,
    pub notes: Vec<String>,
    /// The fonts (as objects) that had text cut out of them.
    pub touched: Vec<ObjRef>,
    /// The text that was taken out, as the runs of characters that went with a line feed before each (for looking for it where it may be left).
    pub removed: String,
}

impl<'a> Redactor<'a> {
    pub fn new(doc: &'a Document) -> Redactor<'a> {
        let mut run = Run::<false>::new(doc, "");
        run.scanner = EditScanner::for_redaction(doc);
        Redactor { run, labels: Vec::new(), widgets: Vec::new(), notes: Vec::new(), touched: Vec::new(), removed: String::new() }
    }

    pub fn scanner(&mut self) -> &mut EditScanner<'a> {
        &mut self.run.scanner
    }

    /// The page, read for redaction (charged to `work`).
    pub fn scan(&mut self, doc: &Document, page: &Page, work: &Work) -> Result<Scan> {
        let _budget = doc.page_decode_budget();
        let scan = self.run.scanner.scan(page, work)?;
        // A content stream that cannot be read is content that cannot be redacted.
        let listed = crate::edit::content::contents_list(doc, page.dict.get("Contents")).len();
        if scan.sources.first().is_some_and(|s| s.streams.len() < listed) {
            return Err(Error::Invalid("a content stream of the page cannot be read (a filter that is not known), so what is under the marks cannot be taken out".to_string()));
        }
        Ok(scan)
    }

    /// What the reading found wrong without stopping (a form that could not be read, a font that is not there).
    pub fn warnings(&mut self) -> Vec<String> {
        self.run.scanner.take_warnings()
    }

    /// Take out of page `index` (from 0; `page`, read as `scan`) what lies under `marks`. The objects are made in `ctx`.
    pub fn apply(&mut self, ctx: &mut Ctx<'_>, index: usize, page: &Page, scan: &Scan, marks: &[Mark]) -> Result<Counts> {
        let doc = ctx.doc;
        let shown = Shown::of(page);
        let zones = zones(&shown, marks)?;
        // Text shown with a font that is not there is not read: where it is, and what, is not known.
        if scan.unreadable > 0 && !zones.is_empty() {
            return Err(Error::Invalid("text is shown in a font that is not in the resources of the page, so it cannot be read and what is under the marks cannot be taken out".to_string()));
        }
        // A standard font that has no /Widths has the widths of a guess: where its characters are is not known.
        if scan.fonts.iter().any(|f| f.font.guessed_widths()) {
            self.notes.push(format!("page {}: a font has no widths (a standard font), so the places of its text are a guess: the lines that a mark crosses were taken out whole; check the page", index + 1));
        }
        let mut edits = PageEdits::default();
        let mut counts = Counts::default();

        // The text.
        let cut = glyphs_cut(scan, &shown, &zones, ctx.work)?;
        counts.glyphs = plan_text(scan, &cut, &mut edits);
        // The fonts that lost text, and the text that went (for looking for it in the places that this does not change).
        for &si in &edits.shows {
            if let Some(r) = scan.shows.get(si as usize).and_then(|s| scan.fonts.get(s.font as usize)).and_then(|f| f.entry.as_obj_ref())
                && !self.touched.contains(&r)
            {
                self.touched.push(r);
            }
        }
        let mut after = usize::MAX;
        for m in scan.marks.iter().filter(|m| cut.get(m.glyph) == Some(&true)) {
            if self.removed.len() > 100_000 {
                break;
            }
            if m.start != after {
                self.removed.push('\n');
            }
            self.removed.push_str(scan.text.get(m.start..m.end).unwrap_or(""));
            after = m.end;
        }

        // The pictures and the paths. (The pictures that are replaced, and the ones that are drawn as they are, by the stream that draws them.)
        let mut replaced: Vec<(u32, ObjRef)> = Vec::new();
        let mut kept: Vec<(u32, ObjRef)> = Vec::new();
        let mut path_gone = vec![false; scan.draws.len()];
        for (di, d) in scan.draws.iter().enumerate() {
            match &d.kind {
                DrawKind::Path(b) => {
                    if zones.iter().any(|z| within(b, &z.user)) {
                        edits.extra.push((d.src, d.start as usize, d.end as usize, b"n".to_vec()));
                        counts.paths += 1;
                        if let Some(slot) = path_gone.get_mut(di) {
                            *slot = true;
                        }
                    }
                }
                DrawKind::Image(_) | DrawKind::Inline => {
                    let unit = bounds_of([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| (d.ctm[0] * u + d.ctm[2] * v + d.ctm[4], d.ctm[1] * u + d.ctm[3] * v + d.ctm[5])));
                    let hit: Vec<&Zone> = zones.iter().filter(|z| meets(&unit, &z.user)).collect();
                    let Some(first) = hit.first() else {
                        if let DrawKind::Image(r) = &d.kind {
                            kept.push((d.src, *r));
                        }
                        continue;
                    };
                    let rects: Vec<[f64; 4]> = hit.iter().map(|z| z.user).collect();
                    let stream = match &d.kind {
                        DrawKind::Image(r) => match doc.get(*r) {
                            Ok(Object::Stream(s)) => Some(s),
                            _ => None,
                        },
                        _ => scan.sources.get(d.src as usize).and_then(|s| s.content.get(d.start as usize..d.end as usize)).and_then(inline_image),
                    };
                    let color = marks.get(first.mark).map_or([0, 0, 0], |m| m.color);
                    let outcome = match &stream {
                        Some(s) => pixels::redact_image(doc, ctx.work, s, &d.ctm, &rects, color)?,
                        None => Outcome::Gone,
                    };
                    if let DrawKind::Image(r) = &d.kind {
                        match outcome {
                            Outcome::Clear => kept.push((d.src, *r)),
                            _ => replaced.push((d.src, *r)),
                        }
                    }
                    match outcome {
                        Outcome::Clear => {}
                        Outcome::Gone => {
                            edits.extra.push((d.src, d.start as usize, d.end as usize, Vec::new()));
                            counts.images += 1;
                        }
                        Outcome::Painted(mut image, mask) => {
                            if let Some(m) = mask {
                                ctx.spend(m.data.len() + 96)?;
                                let r = ctx.alloc()?;
                                ctx.objects.push((r.num, Object::Stream(m)));
                                image.dict.set("SMask", Object::Ref(r));
                            }
                            ctx.spend(image.data.len() + 96)?;
                            let r = ctx.alloc()?;
                            ctx.objects.push((r.num, Object::Stream(image)));
                            let name = free_name(doc, page, scan, d.src, &edits.xobjects, r.num);
                            edits.extra.push((d.src, d.start as usize, d.end as usize, xobject_name(&name).into_bytes()));
                            edits.xobjects.push((d.src, name, r));
                            counts.images += 1;
                        }
                    }
                }
            }
        }

        // A picture that is drawn nowhere as it was is not kept by the names the resources give it.
        for (i, &(src, r)) in replaced.iter().enumerate() {
            if !kept.contains(&(src, r)) && !replaced.get(..i).is_some_and(|before| before.contains(&(src, r))) {
                edits.xobjects.push((src, Vec::new(), r));
            }
        }

        // A pattern that nothing is painted with now (the paths that it painted went) is taken out of the resources, with what it holds.
        for (i, (src, name)) in scan.pat_names.iter().enumerate() {
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            let used = scan.pat_uses.iter().filter(|u| u.0 == i).count();
            if used > 0 && used == scan.pat_uses.iter().filter(|u| u.0 == i && path_gone.get(u.1 as usize) == Some(&true)).count() {
                edits.others.push((*src, "Pattern", name.clone(), ObjRef { num: 0, generation: 0 }));
            }
        }
        // The words the page's marked content carries (`/Alt`, `/E`, `/ActualText`) are read out as the page's own: they go, from the property lists written in
        // the statements and from the ones the resources have.
        edits.spans.extend((0..scan.spans.len()).map(|i| (u32::try_from(i).unwrap_or(u32::MAX), None)));
        for src in 0..scan.sources.len() {
            let lists = effective_resources(doc, page, scan, src).get("Properties").map(|p| resolve_dict(doc, p)).unwrap_or_default();
            for (name, entry) in lists.iter() {
                let Some(list) = without_words(doc, entry) else { continue };
                ctx.spend(list.approx_size() + 48)?;
                if let Object::Ref(r) = entry {
                    ctx.objects.push((r.num, Object::Dict(list)));
                } else {
                    let r = ctx.alloc()?;
                    ctx.objects.push((r.num, Object::Dict(list)));
                    edits.others.push((u32::try_from(src).unwrap_or(u32::MAX), "Properties", name.0.clone(), r));
                }
            }
        }

        // The annotations. What is taken off the page is also taken out of what else may name it (the structure tree names annotations and widgets):
        // each is made a small empty one.
        let (list, removed, taken) = filter_annots(doc, page, &shown, &zones, &mut self.widgets)?;
        counts.annots = removed;
        for r in taken {
            let mut stub = Dict::new();
            stub.set("Type", Object::from("Annot"));
            stub.set("Subtype", Object::from("Text"));
            stub.set("F", Object::Integer(2));
            ctx.objects.push((r.num, Object::Dict(stub)));
        }
        // (A page that has marks has a list to write at least once; the marks are in it.)
        edits.annots = list;

        // The boxes.
        let drawn = overlay_content(&shown, marks, &mut self.labels, &mut self.notes);
        ctx.spend(drawn.len())?;
        edits.overlay = drawn.into_bytes();

        // Every statement must be in a stream that can be written again; a form that is drawn from two places cannot be copied for one.
        edits.seal();
        let mut spots: Vec<(u32, usize, usize)> = Vec::new();
        for &si in &edits.shows {
            if let Some(s) = scan.shows.get(si as usize) {
                spots.push((s.src, s.start as usize, s.end as usize));
            }
        }
        for (id, _) in &edits.spans {
            if let Some(s) = scan.spans.get(*id as usize) {
                spots.push((s.src, s.start as usize, s.end as usize));
            }
        }
        spots.extend(edits.extra.iter().map(|c| (c.0, c.1, c.2)));
        for (src, _, _) in spots {
            // (The page's own content is written as one stream, so where a statement lies in it does not matter; a form must be copied for this page alone.)
            let Some(source) = scan.sources.get(src as usize) else { return Err(Error::Invalid("the page has too many forms to follow".to_string())) };
            if !source.also.is_empty() {
                return Err(Error::Invalid("a form with something under a mark is drawn from two places by different names, so what is under the marks cannot be taken out".to_string()));
            }
        }
        self.run.write_page(ctx, page, scan, &edits)?;
        Ok(counts)
    }
}

/// A property list of marked content (`/Alt`, `/E` and `/ActualText` are read out as the page's words) without them; `None` when it has none of them.
fn without_words(doc: &Document, entry: &Object) -> Option<Dict> {
    let Ok(Object::Dict(mut d)) = doc.resolve(entry) else { return None };
    let found = ["Alt", "E", "ActualText"].map(|k| d.remove(k).is_some());
    found.contains(&true).then_some(d)
}

/// The annotations of the page that stay, and how many go. A mark (a `Redact` annotation) always goes; so does everything whose rectangle meets
/// a mark, and the popup of one that goes. The widgets that go are noted for the form.
fn filter_annots(doc: &Document, page: &Page, shown: &Shown, zones: &[Zone], widgets: &mut Vec<ObjRef>) -> Result<(Option<Vec<Object>>, usize, Vec<ObjRef>)> {
    let Some(Object::Array(items)) = page.dict.get("Annots").and_then(|a| doc.resolve(a).ok()) else { return Ok((None, 0, Vec::new())) };
    let mut gone: Vec<bool> = Vec::with_capacity(items.len());
    let mut removed_refs: Vec<ObjRef> = Vec::new();
    let mut counted = 0usize;
    let mut dicts: Vec<Option<Dict>> = Vec::with_capacity(items.len());
    for item in &items {
        let d = match doc.resolve(item) {
            Ok(Object::Dict(d)) => Some(d),
            _ => None,
        };
        let subtype = d.as_ref().and_then(|d| d.get_name("Subtype")).map(|n| n.as_bytes().to_vec()).unwrap_or_default();
        let marked = subtype == b"Redact";
        let under = !marked && d.as_ref().and_then(|d| rect_of(doc, d)).is_some_and(|r| {
            let b = bounds_of([(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])].map(|(x, y)| shown.map(x, y)));
            zones.iter().any(|z| meets(&b, &z.shown))
        });
        if under && subtype == b"Widget" && let Some(r) = item.as_obj_ref() {
            widgets.push(r);
        }
        if (marked || under) && let Some(r) = item.as_obj_ref() {
            removed_refs.push(r);
        }
        gone.push(marked || under);
        counted += usize::from(under);
        dicts.push(d);
    }
    // The popup of a note that goes.
    for (k, d) in dicts.iter().enumerate() {
        let Some(d) = d else { continue };
        if d.get_name("Subtype").is_some_and(|n| n == "Popup") && d.get("Parent").and_then(Object::as_obj_ref).is_some_and(|p| removed_refs.contains(&p)) && gone.get(k) == Some(&false) {
            if let Some(slot) = gone.get_mut(k) {
                *slot = true;
            }
            counted += 1;
        }
    }
    // A reply (`/IRT`) to a note that goes.
    for (k, d) in dicts.iter().enumerate() {
        if d.as_ref().and_then(|d| d.get("IRT")).and_then(Object::as_obj_ref).is_some_and(|p| removed_refs.contains(&p))
            && let Some(slot) = gone.get_mut(k).filter(|s| !**s)
        {
            *slot = true;
            counted += 1;
        }
    }
    if !gone.contains(&true) {
        return Ok((None, 0, Vec::new()));
    }
    // The ones that are not marks and go, by their numbers (a mark names nothing that matters).
    let taken: Vec<ObjRef> = items.iter().zip(&gone).zip(&dicts).filter(|((_, g), d)| **g && d.as_ref().is_some_and(|d| !d.get_name("Subtype").is_some_and(|n| n == "Redact"))).filter_map(|((o, _), _)| o.as_obj_ref()).collect();
    let kept: Vec<Object> = items.into_iter().zip(&gone).filter(|(_, g)| !**g).map(|(o, _)| o).collect();
    Ok((Some(kept), counted, taken))
}

impl<const MANY: bool> Run<'_, MANY> {
    /// The page's content with `overlay` drawn after it, in the state the page started in: what the content leaves open is closed first, and what
    /// it closes without having opened it is opened in front of it. Nothing is added when there is no overlay.
    pub(super) fn with_overlay(&self, ctx: &mut Ctx<'_>, scan: &Scan, overlay: &[u8], mut list: Vec<Object>) -> Result<Vec<Object>> {
        if overlay.is_empty() {
            return Ok(list);
        }
        let balance = scan.sources.first().map(|s| scan_balance(&s.content)).unwrap_or_default();
        let opens = "q\n".repeat(1 + balance.underflow.min(MAX_BALANCE) as usize);
        let mut closes = String::new();
        if balance.path == Some(true) {
            closes.push_str("n\n");
        }
        if balance.text == Some(true) {
            closes.push_str("ET\n");
        }
        closes.push_str(&"Q\n".repeat(1 + balance.depth.min(MAX_BALANCE) as usize));
        let mut tail = closes.into_bytes();
        tail.extend_from_slice(overlay);
        list.insert(0, Object::Ref(new_stream(ctx, opens.into_bytes())?));
        list.push(Object::Ref(new_stream(ctx, tail)?));
        Ok(list)
    }
}
