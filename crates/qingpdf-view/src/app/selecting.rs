//! The mouse, selected text and copying (3d-2), and the marks drawn over the pages (found words and the selection).

use qingpdf_core::view::{CharBoxes, CharIndex, TextStatus, line_rects};

use super::*;
use crate::select::CharPos;

/// How far (at 96 dpi) the pointer must move with the button down before it is a drag and not a click.
const DRAG_THRESHOLD: i64 = 4;
/// Pages whose boxes of characters are kept, and the most bytes they (and what finds a character in them) may take together. The
/// pages the selection is made between are kept whatever they take: a page has 400,000 characters at most, 6.4 MB.
const BOXES_KEPT: usize = 8;
const BOXES_BYTES: usize = 12 << 20;

/// The boxes of the characters of a page, and what finds the character nearest to a point in them without looking at them all.
pub(super) struct PageBoxes {
    pub(super) boxes: Vec<[f32; 4]>,
    index: CharIndex,
}

impl PageBoxes {
    fn new(boxes: Vec<[f32; 4]>) -> PageBoxes {
        let index = CharIndex::new(&boxes);
        PageBoxes { boxes, index }
    }

    fn bytes(&self) -> usize {
        self.boxes.len() * std::mem::size_of::<[f32; 4]>() + self.index.bytes()
    }

    fn nearest(&self, x: f32, y: f32) -> Option<usize> {
        self.index.nearest(&self.boxes, x, y)
    }
}

/// The left button is down: where it went down (the page, the place on it in points, the place in the window), the link under it
/// if any, and whether the pointer has moved far enough to be a drag.
pub(super) struct Press {
    page: u32,
    x: f64,
    y: f64,
    sx: i64,
    sy: i64,
    link: Option<usize>,
    moved: bool,
}

impl App {
    pub(super) fn mouse_down(&mut self, x: i64, y: i64, _ctrl: bool, _shift: bool) -> Vec<Action> {
        self.mouse = (x, y);
        self.press = None;
        let mut actions = Vec::new();
        if self.selection.take().is_some() {
            actions.push(Action::Invalidate);
        }
        let Some((page, px, py)) = self.page_at_point(x, y) else { return actions };
        let Some(map) = self.doc.as_ref().and_then(|d| d.layout.page_map(page)) else { return actions };
        let (xp, yp) = map.to_pt(px, py);
        let link = self.link_at(page, xp, yp);
        self.press = Some(Press { page: page as u32, x: xp, y: yp, sx: x, sy: y, link, moved: false });
        actions
    }

    pub(super) fn mouse_move(&mut self, x: i64, y: i64) -> Vec<Action> {
        self.mouse = (x, y);
        let threshold = self.px(DRAG_THRESHOLD);
        let Some(press) = self.press.as_mut() else { return Vec::new() };
        let mut actions = Vec::new();
        if !press.moved {
            if (x - press.sx).abs().max((y - press.sy).abs()) < threshold {
                return actions;
            }
            press.moved = true;
            press.link = None;
            actions.push(Action::SetTimer { id: TIMER_DRAG, ms: DRAG_MS });
        }
        actions.extend(self.extend_selection());
        actions
    }

    pub(super) fn mouse_up(&mut self, x: i64, y: i64) -> Vec<Action> {
        self.mouse = (x, y);
        let mut actions = vec![Action::KillTimer(TIMER_DRAG)];
        if let Some(press) = self.press.take()
            && !press.moved
            && let Some(index) = press.link
        {
            actions.extend(self.activate_link(press.page, index));
        }
        actions
    }

    pub(super) fn mouse_lost(&mut self) -> Vec<Action> {
        self.press = None;
        vec![Action::KillTimer(TIMER_DRAG)]
    }

    /// The pointer is held outside the pages while dragging: scroll, and keep the selection with the pointer.
    pub(super) fn drag_timer(&mut self) -> Vec<Action> {
        if !self.press.as_ref().is_some_and(|p| p.moved) {
            return vec![Action::KillTimer(TIMER_DRAG)];
        }
        let (_, y) = self.mouse;
        let speed = self.px(24);
        let dy = if y < 0 {
            -(speed + (-y) / 4)
        } else if y > self.view_h {
            speed + (y - self.view_h) / 4
        } else {
            return Vec::new();
        };
        let mut actions = self.scroll_by(0, dy, true);
        actions.extend(self.extend_selection());
        actions
    }

    /// The place on a page that is under the pointer, or nearest to it (the gap between pages belongs to the page above): the page,
    /// and the place in points.
    fn nearest_page_point(&self, x: i64, y: i64) -> Option<(u32, f32, f32)> {
        let l = &self.doc.as_ref()?.layout;
        let dy = y + self.scroll_y;
        let page = l.page_at(dy);
        let map = l.page_map(page)?;
        let (px, py) = ((x - self.view_x - l.left(page, self.view_w, self.scroll_x)) as f64, (dy - l.top(page)) as f64);
        let (xp, yp) = map.to_pt(px, py);
        Some((page as u32, xp as f32, yp as f32))
    }

    fn ask_for_boxes(&mut self, page: u32) -> Vec<Action> {
        if !self.boxes_asked.values().any(|&p| p == page) {
            let id = self.new_id();
            self.engine.char_boxes(self.doc_id, id, page);
            self.boxes_asked.insert(id, page);
        }
        Vec::new()
    }

    /// Make the selection run from where the button went down to where the pointer is. What is missing (the boxes of a page) is
    /// asked for, and this is done again when it comes.
    pub(super) fn extend_selection(&mut self) -> Vec<Action> {
        let Some((press_page, press_x, press_y)) = self.press.as_ref().filter(|p| p.moved).map(|p| (p.page, p.x, p.y)) else { return Vec::new() };
        if !self.rights.copy {
            let text = t(self.lang, Msg::NoteCopyDenied).to_string();
            return if self.note.as_deref() == Some(text.as_str()) { Vec::new() } else { self.say(text) };
        }
        if self.selection.is_none() {
            let Some(boxes) = self.boxes.get(&press_page) else { return self.ask_for_boxes(press_page) };
            let Some(first) = boxes.nearest(press_x as f32, press_y as f32) else { return Vec::new() };
            let at = CharPos { page: press_page, index: first as u32 };
            self.selection = Some(Selection { anchor: at, head: at });
        }
        let (mx, my) = self.mouse;
        let Some((page, x, y)) = self.nearest_page_point(mx, my) else { return Vec::new() };
        let Some(boxes) = self.boxes.get(&page) else { return self.ask_for_boxes(page) };
        let Some(index) = boxes.nearest(x, y) else { return vec![Action::Invalidate] };
        let head = CharPos { page, index: index as u32 };
        match self.selection.as_mut() {
            Some(sel) if Selection::within_limit(sel.anchor, head) && sel.head != head => {
                sel.head = head;
                vec![Action::Invalidate]
            }
            _ => vec![Action::Invalidate],
        }
    }

    pub(super) fn on_boxes(&mut self, b: CharBoxes) -> Vec<Action> {
        if self.boxes_asked.remove(&b.id).is_none() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        match b.status {
            TextStatus::Ok => {}
            TextStatus::Denied => actions.extend(self.say(t(self.lang, Msg::NoteCopyDenied).to_string())),
            // A page that has no text to give: remembered as having none, so that it is not asked for again.
            TextStatus::Failed => {}
        }
        self.boxes.insert(b.page, PageBoxes::new(b.boxes));
        // Few pages are kept, and not too many bytes of them: the ones nearest the window, and the ends of the selection (and the
        // page that has just come).
        let mut bytes: usize = self.boxes.values().map(PageBoxes::bytes).sum();
        if self.boxes.len() > BOXES_KEPT || bytes > BOXES_BYTES {
            let here = self.current_page() as u32;
            let keep: Vec<u32> = self.selection.iter().flat_map(|s| [s.anchor.page, s.head.page]).chain(self.press.iter().map(|p| p.page)).chain([b.page]).collect();
            let mut pages: Vec<u32> = self.boxes.keys().copied().filter(|p| !keep.contains(p)).collect();
            pages.sort_by_key(|p| std::cmp::Reverse(p.abs_diff(here)));
            for far in pages {
                if self.boxes.len() <= BOXES_KEPT && bytes <= BOXES_BYTES {
                    break;
                }
                if let Some(gone) = self.boxes.remove(&far) {
                    bytes -= gone.bytes();
                }
            }
        }
        actions.extend(self.extend_selection());
        actions.push(Action::Invalidate);
        actions
    }

    // --- copying -----------------------------------------------------------------------------------------------

    pub(super) fn copy_selection(&mut self) -> Vec<Action> {
        if !self.rights.copy {
            return self.say(t(self.lang, Msg::NoteCopyDenied).to_string());
        }
        let Some(sel) = self.selection else { return Vec::new() };
        if let Some(old) = self.copy_asked.take() {
            self.engine.cancel(old);
        }
        let (from, to) = sel.copy_span();
        let id = self.new_id();
        self.engine.copy_text(self.doc_id, id, from, to);
        self.copy_asked = Some(id);
        Vec::new()
    }

    pub(super) fn on_copied(&mut self, id: u64, status: TextStatus, text: String, truncated: bool) -> Vec<Action> {
        if self.copy_asked != Some(id) {
            return Vec::new();
        }
        self.copy_asked = None;
        match status {
            TextStatus::Ok => {
                let mut note = i18n::copied(self.lang, text.chars().count());
                if truncated {
                    note.push_str(" - ");
                    note.push_str(t(self.lang, Msg::NoteCopyCut));
                }
                // What is said waits for the clipboard to answer: it may not take the text.
                self.copy_note = Some(note);
                vec![Action::SetClipboard(text)]
            }
            TextStatus::Denied | TextStatus::Failed => self.say(t(self.lang, Msg::NoteCopyDenied).to_string()),
        }
    }

    /// The clipboard has answered: say the copy was made, or that it was not.
    pub(super) fn on_clipboard(&mut self, ok: bool) -> Vec<Action> {
        match self.copy_note.take() {
            Some(note) if ok => self.say(note),
            _ if !ok => self.say(t(self.lang, Msg::NoteCopyFailed).to_string()),
            _ => Vec::new(),
        }
    }

    // --- marks over the pages ------------------------------------------------------------------------------------

    /// Colour the found words and the selected text of a page, whose top left corner is at (`left`, `top`) in the window.
    pub(super) fn draw_marks(&self, p: &mut dyn Painter, page: usize, left: i64, top: i64) {
        let Some(map) = self.doc.as_ref().and_then(|d| d.layout.page_map(page)) else { return };
        let area = |r: (f64, f64, f64, f64)| {
            let (x0, y0) = (r.0.floor() as i64, r.1.floor() as i64);
            let (x1, y1) = (r.2.ceil() as i64, r.3.ceil() as i64);
            Rect { x: clamp32(left + x0), y: clamp32(top + y0), w: clamp32((x1 - x0).max(1)), h: clamp32((y1 - y0).max(1)) }
        };
        if let Some(s) = &self.search {
            for (i, hit) in s.page_hits(page as u32).iter().enumerate() {
                let current = s.current.is_some_and(|c| c.page == page as u32 && c.index == i);
                let (color, alpha) = if current { (CURRENT_HIT_COLOR, CURRENT_HIT_ALPHA) } else { (HIT_COLOR, HIT_ALPHA) };
                for r in &hit.rects {
                    p.fill_rect_alpha(area(map.rect_to_px(*r)), color, alpha);
                }
            }
        }
        if let Some(sel) = &self.selection
            && let Some(boxes) = self.boxes.get(&(page as u32))
            && let Some((a, b)) = sel.range_in(page as u32, boxes.boxes.len())
            && let Some(chars) = boxes.boxes.get(a..b)
        {
            for r in line_rects(chars) {
                p.fill_rect_alpha(area(map.rect_to_px(r)), SELECTION_COLOR, SELECTION_ALPHA);
            }
        }
    }
}
