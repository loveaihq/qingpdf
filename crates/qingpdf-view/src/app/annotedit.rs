//! Shapes, and the annotations that are on the page picked, moved, resized, drawn again in another colour or line width and given a new text
//! (2026-10-17). The reader only asks: the engine draws the shape, changes the annotation and keeps the history (`docs/view-api.md`); what is here is
//! the pointer (dragging a shape out, taking hold of the annotation picked by its body or a corner), the arrow keys that nudge it, the frame and
//! the handles drawn round it, and the double click that opens its text.

use qingpdf_core::view::{AnnotInfo, ShapeKind, keep_on_page};

use super::editing::{Asked, COLORS, Tool, WIDTHS};
use super::*;

/// How far (in points) from a line a click still picks it; the side of the handles at the corners (pixels at 96 dpi); how far (pixels at 96 dpi)
/// the pointer must go with the button down before it moves what it holds.
const LINE_SLOP: f32 = 4.0;
const GRIP: i64 = 9;
const MOVE_AFTER: i64 = 4;
/// A shape dragged less far than this (pixels at 96 dpi) is a click, which draws nothing.
const SHAPE_AFTER: i64 = 5;
/// The least a box may be made by dragging a corner (points).
const LEAST_SIDE: f32 = 8.0;
/// How long the arrow keys are waited for before the nudges are sent as one move (so that holding a key is one step to undo, not a hundred).
pub(super) const NUDGE_MS: u32 = 400;
/// A shape being dragged out: the page, where the pointer went down and where it is (points of the page), where it went down in the window
/// (pixels) and whether Shift is held.
pub(super) struct ShapeDraft {
    page: u32,
    from: [f32; 2],
    to: [f32; 2],
    px: (i64, i64),
    shift: bool,
}

/// What of the annotation picked the pointer holds: its body, or a corner (0 top left, 1 top right, 2 bottom left, 3 bottom right).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    Body,
    Corner(usize),
}

/// The annotation the pointer holds: which one, where the pointer went down (points and pixels), the rectangle it had and the one it is being
/// made.
struct Held {
    page: u32,
    index: u32,
    num: u32,
    grip: Grip,
    /// A picture keeps its shape as it is resized.
    keep_shape: bool,
    start: [f32; 2],
    px: (i64, i64),
    at: [f32; 4],
    now: [f32; 4],
    moved: bool,
}

/// Arrow keys pressed and not yet sent: the annotation and the distance in points.
struct Nudge {
    page: u32,
    index: u32,
    num: u32,
    at: [f32; 4],
    by: (f32, f32),
}

pub(super) struct AnnotEdit {
    /// Rectangles and ellipses are filled (the Comment menu's switch).
    pub fill: bool,
    draft: Option<ShapeDraft>,
    held: Option<Held>,
    nudge: Option<Nudge>,
    /// The whole text of an annotation was asked for, to change it: the request, the page and the place in its list, the number.
    pub(super) text_ask: Option<(u64, u32, u32, u32)>,
    /// The note whose text is asked for in the box: page, place, number and the text it had.
    note: Option<(u32, u32, u32, String)>,
    /// The annotation to pick when its page's list is here (after a change to it, or a click in the list of the sidebar): page, place, number.
    pub want_pick: Option<(u32, u32, u32)>,
}

impl AnnotEdit {
    pub(super) fn new() -> AnnotEdit {
        AnnotEdit { fill: false, draft: None, held: None, nudge: None, text_ask: None, note: None, want_pick: None }
    }

    /// The pointer was lost, or Esc: whatever was being dragged is given up.
    pub(super) fn give_up(&mut self) {
        self.draft = None;
        self.held = None;
    }

    /// The pages are not the pages they were.
    pub(super) fn forget_pages(&mut self) {
        *self = AnnotEdit { fill: self.fill, ..AnnotEdit::new() };
    }

    pub(super) fn holds(&self) -> bool {
        self.draft.is_some() || self.held.is_some()
    }
}

/// Is this a mark on text (picked by clicking, but dragged over is for selecting the text under it)?
fn is_text_markup(a: &AnnotInfo) -> bool {
    matches!(a.subtype.as_str(), "Highlight" | "Underline" | "StrikeOut" | "Squiggly")
}

/// The distance from the point to the segment.
fn distance(p: (f32, f32), [x0, y0, x1, y1]: [f32; 4]) -> f32 {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - x0) * dx + (p.1 - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    ((p.0 - (x0 + t * dx)).hypot(p.1 - (y0 + t * dy))).abs()
}

/// Does the annotation hold the point (x, y) of its page? A line is picked by the line, a mark on text by its lines, the rest by the rectangle.
fn hits(a: &AnnotInfo, x: f32, y: f32) -> bool {
    if let Some(l) = a.line {
        return distance((x, y), l) <= LINE_SLOP;
    }
    let rects: &[[f32; 4]] = if a.parts.is_empty() { std::slice::from_ref(&a.rect) } else { &a.parts };
    rects.iter().any(|r| r[0] - 2.0 <= x && x <= r[2] + 2.0 && r[1] - 2.0 <= y && y <= r[3] + 2.0)
}

/// The two corners of a shape as the shift key leaves them: a square or circle, a line at a multiple of 45 degrees.
fn constrained(kind: ShapeKind, from: [f32; 2], to: [f32; 2], shift: bool) -> [f32; 2] {
    if !shift {
        return to;
    }
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    match kind {
        ShapeKind::Rectangle | ShapeKind::Ellipse => {
            let s = dx.abs().max(dy.abs());
            [from[0] + s * if dx < 0.0 { -1.0 } else { 1.0 }, from[1] + s * if dy < 0.0 { -1.0 } else { 1.0 }]
        }
        ShapeKind::Line | ShapeKind::Arrow => {
            let step = std::f32::consts::FRAC_PI_4;
            let angle = (dy.atan2(dx) / step).round() * step;
            let len = dx.hypot(dy);
            [from[0] + len * angle.cos(), from[1] + len * angle.sin()]
        }
    }
}

/// The lines a shape is previewed with, as points of the page.
fn preview(kind: ShapeKind, a: [f32; 2], b: [f32; 2]) -> Vec<Vec<[f32; 2]>> {
    match kind {
        ShapeKind::Rectangle => vec![vec![a, [b[0], a[1]], b, [a[0], b[1]], a]],
        ShapeKind::Ellipse => {
            let (cx, cy, rx, ry) = ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (b[0] - a[0]) / 2.0, (b[1] - a[1]) / 2.0);
            vec![(0..=64).map(|i| {
                let t = i as f32 / 64.0 * std::f32::consts::TAU;
                [cx + rx * t.cos(), cy + ry * t.sin()]
            }).collect()]
        }
        ShapeKind::Line => vec![vec![a, b]],
        ShapeKind::Arrow => {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dy).max(1e-3);
            let (ux, uy) = (dx / len, dy / len);
            let barb = (len * 0.4).clamp(6.0, 14.0);
            let wing = |s: f32| [b[0] - barb * (0.88 * ux - 0.48 * s * uy), b[1] - barb * (0.88 * uy + 0.48 * s * ux)];
            vec![vec![a, b], vec![wing(1.0), b, wing(-1.0)]]
        }
    }
}

impl App {
    // --- picking -----------------------------------------------------------------------------------------------------

    /// The annotation of `page` that holds the point (x, y) of it in points: the last of its list (it is on top).
    pub(super) fn annotation_at(&self, page: u32, x: f32, y: f32) -> Option<usize> {
        self.edit.annots.get(&page)?.iter().rposition(|a| hits(a, x, y))
    }

    /// The status line for the annotation just picked.
    pub(super) fn describe_picked(&mut self) -> Vec<Action> {
        let Some(a) = self.edit.picked_info() else { return Vec::new() };
        let mut text = self.kind_name(&a.subtype);
        if !a.author.is_empty() {
            text.push_str(" - ");
            text.push_str(&a.author);
        }
        if !a.contents.is_empty() {
            text.push_str(": ");
            text.push_str(&a.contents);
        }
        self.say(text)
    }

    /// The annotations of `page` have come (again): the one that is wanted is picked.
    pub(super) fn resolve_want_pick(&mut self, page: u32) {
        let Some((p, index, num)) = self.edit.ae.want_pick.filter(|w| w.0 == page) else { return };
        let Some(list) = self.edit.annots.get(&page) else { return };
        self.edit.picked = list.iter().position(|a| a.index == index && a.num == num).map(|i| (p, i));
        self.edit.ae.want_pick = None;
        self.menus_dirty = true;
    }

    /// The corner of the window rectangle `r` that the place (x, y) of the window is on, if it is on one.
    fn corner_at(&self, r: Rect, x: i64, y: i64) -> Option<usize> {
        let near = self.px(GRIP) / 2 + self.px(3);
        let corners = [(r.x, r.y), (r.x + r.w, r.y), (r.x, r.y + r.h), (r.x + r.w, r.y + r.h)];
        corners.iter().position(|&(cx, cy)| (x - i64::from(cx)).abs() <= near && (y - i64::from(cy)).abs() <= near)
    }

    // --- the pointer ---------------------------------------------------------------------------------------------------

    /// The pointer went down with the select tool: the annotation picked is taken hold of (a corner resizes it, the rest moves it; a double click
    /// opens its text), or one that is under the pointer is picked and taken hold of. `None`: this is not for annotations (the old ways: a link, a
    /// field, text, a mark on text picked when the button comes up).
    pub(super) fn annot_mouse_down(&mut self, x: i64, y: i64, clicks: u8) -> Option<Vec<Action>> {
        if self.edit.tool != Tool::Select {
            return None;
        }
        let (page, px, py) = self.page_at_point(x, y)?;
        let (xp, yp) = self.doc.as_ref()?.layout.page_map(page)?.to_pt(px, py);
        let page = page as u32;
        let (xf, yf) = (xp as f32, yp as f32);
        // (A change that is not answered yet is of an annotation whose place is not known: the press waits for the answer.)
        if self.edit.asked.values().any(|a| *a == Asked::Change) {
            return Some(Vec::new());
        }
        let top = self.annotation_at(page, xf, yf);
        let picked = self.edit.picked.filter(|p| p.0 == page).and_then(|p| Some((p.1, self.edit.picked_info()?.clone())));
        if let Some((at, p)) = picked {
            let corner = if self.rights.annotate && !p.locked() && p.resizable() { self.window_rect(page, p.rect).and_then(|r| self.corner_at(r, x, y)) } else { None };
            // The picked one is held where it is the top one under the pointer; a corner of it is held anywhere.
            if corner.is_some() || top == Some(at) {
                return Some(self.take_hold(page, &p, corner.map_or(Grip::Body, Grip::Corner), clicks, x, y));
            }
        }
        // The page's own links and fields come first.
        if self.link_at(page as usize, xp, yp).is_some() || self.field_at(page, xp, yp).is_some() {
            return None;
        }
        let i = top?;
        let found = self.edit.annots.get(&page)?.get(i)?.clone();
        // A mark on text: dragging over it selects the text; a click picks it when the button comes up.
        if is_text_markup(&found) {
            return None;
        }
        self.edit.picked = Some((page, i));
        self.menus_dirty = true;
        let mut actions = vec![Action::Invalidate];
        actions.extend(self.describe_picked());
        actions.extend(self.take_hold(page, &found, Grip::Body, clicks, x, y));
        Some(actions)
    }

    /// Take hold of the annotation `a` by `grip`; a double click on a note or a text box opens its text instead.
    fn take_hold(&mut self, page: u32, a: &AnnotInfo, grip: Grip, clicks: u8, x: i64, y: i64) -> Vec<Action> {
        self.selection = None;
        if clicks >= 2 && a.typed() && grip == Grip::Body {
            return self.ask_annotation_text(page, a);
        }
        if !self.rights.annotate || a.locked() {
            return if a.locked() { self.say(t(self.lang, Msg::NoteLocked).to_string()) } else { Vec::new() };
        }
        let start = self.page_point(page, x, y).map_or([0.0, 0.0], |(a, b)| [a, b]);
        self.edit.ae.held = Some(Held { page, index: a.index, num: a.num, grip, keep_shape: a.subtype == "Stamp", start, px: (x, y), at: a.rect, now: a.rect, moved: false });
        Vec::new()
    }

    /// The pointer went down with a shape tool: the shape is begun.
    pub(super) fn shape_mouse_down(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        let Tool::Shape(_) = self.edit.tool else { return None };
        let (page, ..) = self.page_at_point(x, y)?;
        let (xp, yp) = self.page_point(page as u32, x, y)?;
        self.edit.ae.draft = Some(ShapeDraft { page: page as u32, from: [xp, yp], to: [xp, yp], px: (x, y), shift: false });
        self.selection = None;
        Some(Vec::new())
    }

    /// The pointer moved with the button down: a shape grows, or what is held is moved or resized (its frame, until the button comes up).
    pub(super) fn annot_mouse_move(&mut self, x: i64, y: i64, shift: bool) -> Option<Vec<Action>> {
        if let Some(d) = self.edit.ae.draft.as_ref() {
            let (xp, yp) = self.page_point(d.page, x, y)?;
            if let Some(d) = self.edit.ae.draft.as_mut() {
                d.to = [xp, yp];
                d.shift = shift;
            }
            return Some(vec![Action::Invalidate]);
        }
        let h = self.edit.ae.held.as_ref()?;
        let (page, grip, start, at, keep_shape, press) = (h.page, h.grip, h.start, h.at, h.keep_shape, h.px);
        if !h.moved && (x - press.0).abs().max((y - press.1).abs()) < self.px(MOVE_AFTER) {
            return Some(Vec::new());
        }
        let (xp, yp) = self.page_point(page, x, y)?;
        let (pw, ph) = self.doc.as_ref()?.layout.page_size_pt(page as usize)?;
        let now = match grip {
            Grip::Body => {
                let (dx, dy) = keep_on_page(at.map(f64::from), (f64::from(xp - start[0]), f64::from(yp - start[1])), (pw, ph));
                [at[0] + dx as f32, at[1] + dy as f32, at[2] + dx as f32, at[3] + dy as f32]
            }
            Grip::Corner(c) => {
                // The opposite corner stays where it is; the pointer is the corner that moves (and may go over the other side of the one that stays).
                let (ax, ay) = (if c % 2 == 0 { at[2] } else { at[0] }, if c < 2 { at[3] } else { at[1] });
                let (sx, sy) = (if xp < ax { -1.0 } else { 1.0 }, if yp < ay { -1.0 } else { 1.0 });
                let (mut w, mut hgt) = ((xp - ax).abs().max(LEAST_SIDE), (yp - ay).abs().max(LEAST_SIDE));
                if keep_shape || shift {
                    let ratio = ((at[2] - at[0]) / (at[3] - at[1]).max(1e-3)).max(1e-3);
                    let room = (if sx < 0.0 { ax } else { pw as f32 - ax }).min((if sy < 0.0 { ay } else { ph as f32 - ay }) * ratio);
                    w = w.max(hgt * ratio).min(room.max(LEAST_SIDE));
                    hgt = w / ratio;
                }
                let (x1, y1) = (ax + sx * w, ay + sy * hgt);
                [ax.min(x1), ay.min(y1), ax.max(x1), ay.max(y1)]
            }
        };
        if let Some(h) = self.edit.ae.held.as_mut() {
            h.now = now;
            h.moved = true;
        }
        Some(vec![Action::Invalidate])
    }

    /// The button came up: a shape is made, or what was held is moved or resized in the engine. `None`: nothing of this was going on.
    pub(super) fn annot_mouse_up(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        if let Some(d) = self.edit.ae.draft.take() {
            let Tool::Shape(kind) = self.edit.tool else { return Some(Vec::new()) };
            if (x - d.px.0).abs().max((y - d.px.1).abs()) < self.px(SHAPE_AFTER) {
                return Some(vec![Action::Invalidate]);
            }
            if !self.rights.annotate {
                return Some(self.say(t(self.lang, Msg::NoteAnnotDenied).to_string()));
            }
            if let Some(wait) = self.edit_wait() {
                return Some(wait);
            }
            let to = constrained(kind, d.from, d.to, d.shift);
            let id = self.new_id();
            let width = WIDTHS.get(self.edit.width).map_or(2.0, |w| w.1);
            self.engine.add_shape(self.doc_id, id, d.page, kind, d.from, to, self.rgb(), width, self.edit.ae.fill, &self.edit.author);
            self.edit.asked.insert(id, Asked::Add);
            return Some(vec![Action::Invalidate]);
        }
        let h = self.edit.ae.held.take()?;
        if !h.moved {
            return Some(vec![Action::Invalidate]);
        }
        if let Some(wait) = self.edit_wait() {
            return Some(wait);
        }
        let id = self.new_id();
        match h.grip {
            Grip::Body => self.engine.move_annotation(self.doc_id, id, h.page, h.index, h.num, [h.now[0] - h.at[0], h.now[1] - h.at[1]]),
            Grip::Corner(_) => self.engine.resize_annotation(self.doc_id, id, h.page, h.index, h.num, h.now),
        }
        self.sent_change(id, h.page, h.index, h.num);
        Some(vec![Action::Invalidate])
    }

    /// A change to an annotation was asked of the engine: the annotation is picked again when it is done.
    fn sent_change(&mut self, id: u64, page: u32, index: u32, num: u32) {
        self.edit.asked.insert(id, Asked::Change);
        self.edit.ae.want_pick = Some((page, index, num));
    }

    /// The shape of the pointer over the annotation picked (a corner, the body), or while it is held.
    pub(super) fn pick_cursor(&self, x: i64, y: i64) -> Option<Cursor> {
        let corner_shape = |c: usize| if c == 0 || c == 3 { Cursor::SizeNWSE } else { Cursor::SizeNESW };
        if let Some(h) = self.edit.ae.held.as_ref() {
            return Some(match h.grip {
                Grip::Body => Cursor::Move,
                Grip::Corner(c) => corner_shape(c),
            });
        }
        if self.edit.tool != Tool::Select || !self.rights.annotate {
            return None;
        }
        let (page, px, py) = self.page_at_point(x, y)?;
        let (xp, yp) = self.doc.as_ref()?.layout.page_map(page)?.to_pt(px, py);
        let p = self.edit.picked.filter(|p| p.0 as usize == page).and_then(|_| self.edit.picked_info())?;
        if p.locked() {
            return None;
        }
        if p.resizable()
            && let Some(c) = self.window_rect(page as u32, p.rect).and_then(|r| self.corner_at(r, x, y))
        {
            return Some(corner_shape(c));
        }
        hits(p, xp as f32, yp as f32).then_some(Cursor::Move)
    }

    // --- the arrow keys ------------------------------------------------------------------------------------------------

    /// An arrow key with an annotation picked: it is nudged a point (ten with Shift). `None`: the key is for scrolling.
    pub(super) fn nudge_key(&mut self, key: u32, shift: bool) -> Option<Vec<Action>> {
        let (page, _) = self.edit.picked.filter(|_| self.edit.tool == Tool::Select && self.rights.annotate && !self.thumb_focus && !self.annots_focus)?;
        let a = self.edit.picked_info().filter(|a| !a.locked())?.clone();
        let map = self.doc.as_ref()?.layout.page_map(page as usize)?;
        // The arrow goes the way it points on the screen: the page may be turned.
        let (screen_x, screen_y) = match key {
            vk::LEFT => (-100.0, 0.0),
            vk::RIGHT => (100.0, 0.0),
            vk::UP => (0.0, -100.0),
            _ => (0.0, 100.0),
        };
        let (o, p) = (map.to_pt(0.0, 0.0), map.to_pt(screen_x, screen_y));
        let (vx, vy) = (p.0 - o.0, p.1 - o.1);
        let step = if shift { 10.0 } else { 1.0 };
        let (dx, dy) = if vx.abs() > vy.abs() { (step * vx.signum() as f32, 0.0) } else { (0.0, step * vy.signum() as f32) };
        let same = self.edit.ae.nudge.as_ref().is_some_and(|n| n.page == page && n.index == a.index && n.num == a.num);
        let mut actions = if same { Vec::new() } else { self.flush_nudge() };
        let (pw, ph) = self.doc.as_ref()?.layout.page_size_pt(page as usize)?;
        let n = self.edit.ae.nudge.get_or_insert(Nudge { page, index: a.index, num: a.num, at: a.rect, by: (0.0, 0.0) });
        let (bx, by) = keep_on_page(n.at.map(f64::from), (f64::from(n.by.0 + dx), f64::from(n.by.1 + dy)), (pw, ph));
        n.by = (bx as f32, by as f32);
        actions.push(Action::SetTimer { id: TIMER_NUDGE, ms: NUDGE_MS });
        actions.push(Action::Invalidate);
        Some(actions)
    }

    /// The nudges so far are sent to the engine as one move.
    pub(super) fn flush_nudge(&mut self) -> Vec<Action> {
        let Some(n) = self.edit.ae.nudge.take() else { return Vec::new() };
        let mut actions = vec![Action::KillTimer(TIMER_NUDGE), Action::Invalidate];
        if n.by == (0.0, 0.0) {
            return actions;
        }
        if let Some(wait) = self.edit_wait() {
            actions.extend(wait);
            return actions;
        }
        let id = self.new_id();
        self.engine.move_annotation(self.doc_id, id, n.page, n.index, n.num, [n.by.0, n.by.1]);
        self.sent_change(id, n.page, n.index, n.num);
        actions
    }

    // --- colour, width, fill ---------------------------------------------------------------------------------------------

    /// The annotation picked is drawn again in the colour (or with the line width, or filled or not) the menu chose. `None`: nothing is picked, so
    /// the choice is for the next ones.
    pub(super) fn restyle_picked(&mut self, color: Option<usize>, width: Option<usize>, fill: Option<bool>) -> Option<Vec<Action>> {
        let a = self.edit.picked_info().cloned()?;
        let (page, _) = self.edit.picked?;
        if !self.rights.annotate || a.locked() || !a.restylable() {
            return Some(self.say(t(self.lang, if a.locked() { Msg::NoteLocked } else { Msg::NoteAnnotDenied }).to_string()));
        }
        if let Some(wait) = self.edit_wait() {
            return Some(wait);
        }
        let id = self.new_id();
        let rgb = color.and_then(|c| COLORS.get(c)).map(|c| c.1);
        let pen = width.and_then(|w| WIDTHS.get(w)).map(|w| w.1);
        self.engine.restyle_annotation(self.doc_id, id, page, a.index, a.num, rgb, pen, fill);
        self.sent_change(id, page, a.index, a.num);
        Some(Vec::new())
    }

    /// The Comment menu's fill switch: the shape picked is filled or not, or the next ones are.
    pub(super) fn toggle_fill(&mut self) -> Vec<Action> {
        let fill = !self.edit.ae.fill;
        if self.has_picked()
            && let Some(actions) = self.restyle_picked(None, None, Some(fill))
        {
            return actions;
        }
        self.edit.ae.fill = fill;
        self.menus_dirty = true;
        Vec::new()
    }

    // --- the text of a note or a text box ----------------------------------------------------------------------------------

    /// A double click on a note or a text box: its whole text is asked for, to be typed over.
    fn ask_annotation_text(&mut self, page: u32, a: &AnnotInfo) -> Vec<Action> {
        if !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        if a.text_locked() {
            return self.say(t(self.lang, Msg::NoteLocked).to_string());
        }
        let id = self.new_id();
        self.engine.annotation_text(self.doc_id, id, page, a.index, a.num);
        self.edit.ae.text_ask = Some((id, page, a.index, a.num));
        Vec::new()
    }

    /// The text of the annotation has come: a note's is typed over in the box that asks for a note's text, a text box's in place.
    pub(super) fn on_annot_text(&mut self, id: u64, found: bool, text: String) -> Vec<Action> {
        let Some((_, page, index, num)) = self.edit.ae.text_ask.take_if(|a| a.0 == id) else { return Vec::new() };
        let Some(a) = self.edit.annots.get(&page).and_then(|l| l.iter().find(|a| a.index == index && a.num == num)).cloned() else { return Vec::new() };
        if !found {
            return self.say(t(self.lang, Msg::NoteEditFailed).to_string());
        }
        // A text box is typed over in place; so is a note that has line breaks (the box for notes has one line, and would lose them).
        if a.subtype == "FreeText" || text.contains('\n') {
            let (pw, ph) = self.doc.as_ref().and_then(|d| d.layout.page_size_pt(page as usize)).unwrap_or((600.0, 800.0));
            let rect = if a.subtype == "FreeText" { a.rect } else { [a.rect[0], a.rect[1], (a.rect[0] + 200.0).min(pw as f32), (a.rect[1] + 90.0).min(ph as f32)] };
            return self.open_annotation_box(page, rect, text, index, num);
        }
        self.edit.ae.note = Some((page, index, num, text.clone()));
        vec![Action::AskText {
            tag: TAG_NOTE_EDIT,
            title: t(self.lang, Msg::NoteTitle).to_string(),
            prompt: t(self.lang, Msg::NotePrompt).to_string(),
            secret: false,
            initial: text.replace(['\r', '\n'], " "),
            ok: t(self.lang, Msg::Ok).to_string(),
            cancel: t(self.lang, Msg::Cancel).to_string(),
        }]
    }

    /// The new text of the note was typed (or the box cancelled).
    pub(super) fn note_edit_answered(&mut self, text: Option<String>) -> Vec<Action> {
        let Some((page, index, num, old)) = self.edit.ae.note.take() else { return Vec::new() };
        let Some(text) = text.filter(|t| !t.trim().is_empty() && *t != old.replace(['\r', '\n'], " ")) else { return Vec::new() };
        self.send_text(page, index, num, &text)
    }

    /// Give the annotation a new text.
    pub(super) fn send_text(&mut self, page: u32, index: u32, num: u32, text: &str) -> Vec<Action> {
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let id = self.new_id();
        self.engine.set_annotation_text(self.doc_id, id, page, index, num, text);
        self.sent_change(id, page, index, num);
        Vec::new()
    }

    // --- drawing -------------------------------------------------------------------------------------------------------------

    /// The shape being dragged out, the frame of the annotation picked (with handles at its corners when it can be resized), or the frame it is
    /// being given, on a page whose top left corner is at (`left`, `top`).
    pub(super) fn draw_annot_marks(&self, p: &mut dyn Painter, page: usize, left: i64, top: i64) {
        let Some(map) = self.doc.as_ref().and_then(|d| d.layout.page_map(page)) else { return };
        let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w.max(1)), h: clamp32(h.max(1)) };
        let to_window = |r: [f32; 4]| {
            let (x0, y0, x1, y1) = map.rect_to_px(r);
            (left + x0.floor() as i64 - 1, top + y0.floor() as i64 - 1, left + x1.ceil() as i64 + 1, top + y1.ceil() as i64 + 1)
        };
        let frame = |p: &mut dyn Painter, (x0, y0, x1, y1): (i64, i64, i64, i64), thick: i64| {
            p.fill_rect(rect(x0, y0, x1 - x0, thick), self.frame_color());
            p.fill_rect(rect(x0, y1 - thick, x1 - x0, thick), self.frame_color());
            p.fill_rect(rect(x0, y0, thick, y1 - y0), self.frame_color());
            p.fill_rect(rect(x1 - thick, y0, thick, y1 - y0), self.frame_color());
        };
        if let (Some(d), Tool::Shape(kind)) = (self.edit.ae.draft.as_ref().filter(|d| d.page as usize == page), self.edit.tool) {
            let width = WIDTHS.get(self.edit.width).map_or(2.0, |w| w.1);
            let side = (f64::from(width) * self.doc.as_ref().map_or(1.0, |d| d.layout.scale())).round().max(1.0) as i64;
            let [r, g, b] = self.rgb();
            let color = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            let to = constrained(kind, d.from, d.to, d.shift);
            let (a, c) = ([d.from[0].min(to[0]), d.from[1].min(to[1])], [d.from[0].max(to[0]), d.from[1].max(to[1])]);
            let corners = if matches!(kind, ShapeKind::Rectangle | ShapeKind::Ellipse) { (a, c) } else { (d.from, to) };
            for line in preview(kind, corners.0, corners.1) {
                let points: Vec<(f64, f64)> = line.iter().map(|q| map.to_px(f64::from(q[0]), f64::from(q[1]))).collect();
                self.draw_dots(p, &points, left, top, side, color);
            }
        }
        if self.edit.picked.is_none_or(|p| p.0 as usize != page) {
            return;
        }
        let Some(a) = self.edit.picked_info() else { return };
        let moving = self.edit.ae.held.as_ref().filter(|h| h.moved && h.page as usize == page && h.index == a.index && h.num == a.num);
        let nudged = self.edit.ae.nudge.as_ref().filter(|n| n.page as usize == page && n.index == a.index && n.num == a.num);
        let t = self.px(2).max(1);
        if let Some(h) = moving {
            frame(p, to_window(h.now), t);
        } else if let Some(n) = nudged {
            frame(p, to_window([n.at[0] + n.by.0, n.at[1] + n.by.1, n.at[2] + n.by.0, n.at[3] + n.by.1]), t);
        } else {
            let rects: Vec<[f32; 4]> = if a.parts.is_empty() { vec![a.rect] } else { a.parts.clone() };
            for r in rects {
                frame(p, to_window(r), t);
            }
        }
        // The handles: white squares with a frame, at the corners of the rectangle (where it is being made, while it is dragged).
        if self.rights.annotate && !a.locked() && a.resizable() {
            let (x0, y0, x1, y1) = to_window(moving.map_or(a.rect, |h| h.now));
            let g = self.px(GRIP);
            for (cx, cy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                p.fill_rect(rect(cx - g / 2, cy - g / 2, g, g), 0xFFFFFF);
                frame(p, (cx - g / 2, cy - g / 2, cx - g / 2 + g, cy - g / 2 + g), t.min(2));
            }
        }
    }

    /// Dots along the lines through `points` (pixels from the top left of the page), `side` pixels across: how a line being drawn is shown.
    pub(super) fn draw_dots(&self, p: &mut dyn Painter, points: &[(f64, f64)], left: i64, top: i64, side: i64, color: u32) {
        let dot = |p: &mut dyn Painter, x: f64, y: f64| {
            p.fill_rect(Rect { x: clamp32(left + x.round() as i64 - side / 2), y: clamp32(top + y.round() as i64 - side / 2), w: clamp32(side), h: clamp32(side) }, color);
        };
        let mut last: Option<(f64, f64)> = None;
        for &(x, y) in points {
            if let Some((lx, ly)) = last {
                // Dots along the segment, as many as keep it unbroken.
                let steps = ((x - lx).abs().max((y - ly).abs()) / (side as f64 / 2.0).max(1.0)).ceil().clamp(1.0, 4000.0) as i64;
                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    dot(p, lx + (x - lx) * t, ly + (y - ly) * t);
                }
            } else {
                dot(p, x, y);
            }
            last = Some((x, y));
        }
    }
}
