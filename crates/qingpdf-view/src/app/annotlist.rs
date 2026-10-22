//! The third tab of the sidebar (2026-10-17): every annotation of the file, a heading for each page and under it a row for each annotation with
//! its kind, its author, the first line of its text and when it was changed. A click goes to the page and picks the annotation; Delete takes the
//! one that is picked out (not a locked one). The list is virtual like the thumbnails: a row is a few numbers, only the rows in view are drawn, so
//! thousands of annotations cost nothing. The engine makes the list (`Engine::annot_list`, a page at a time between the other work) and it is asked
//! for again whenever an edit has changed the file and the tab is in view.

use qingpdf_core::view::ListedAnnot;

use super::editing::Asked;
use super::sidebar::fit;
use super::*;
use crate::thumbs::{self, ListKey};
use crate::cmdbar::plain_label;

// Sizes at 96 dpi.
const HEAD_H: i64 = 26;
const ITEM_H: i64 = 52;
const PAD: i64 = 8;
const BAR_W: i64 = 6;
const BAR_EDGE: i64 = 3;
const GUTTER: i64 = 12;

/// One row of the list: the heading of a page, or an annotation (its place in the list of all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Page(u32),
    Item(u32),
}

pub(super) struct AnnotList {
    items: Vec<ListedAnnot>,
    rows: Vec<Row>,
    /// The top of each row, and the height of all, in pixels at 96 dpi.
    tops: Vec<i64>,
    total: i64,
    /// How far the list is scrolled, in pixels.
    pub(super) scroll: i64,
    /// The request for the list that is out, and whether the list has to be asked for (never asked, or the file has changed since).
    asked: Option<u64>,
    stale: bool,
    loaded: bool,
    truncated: bool,
    /// The annotation chosen: page, place in the page's list, object number.
    selected: Option<(u32, u32, u32)>,
    dragging: bool,
}

impl AnnotList {
    pub(super) fn new() -> AnnotList {
        AnnotList { items: Vec::new(), rows: Vec::new(), tops: Vec::new(), total: 0, scroll: 0, asked: None, stale: true, loaded: false, truncated: false, selected: None, dragging: false }
    }

    /// Put the list the engine made in place of the old one: the rows, and where they are.
    fn fill(&mut self, items: Vec<ListedAnnot>, truncated: bool) {
        self.rows.clear();
        self.tops.clear();
        let mut top = 0i64;
        let mut page = None;
        for (i, it) in items.iter().enumerate() {
            if page != Some(it.page) {
                page = Some(it.page);
                self.rows.push(Row::Page(it.page));
                self.tops.push(top);
                top += HEAD_H;
            }
            self.rows.push(Row::Item(u32::try_from(i).unwrap_or(u32::MAX)));
            self.tops.push(top);
            top += ITEM_H;
        }
        self.items = items;
        self.total = top;
        self.truncated = truncated;
        self.loaded = true;
    }

    fn item_row(&self, key: (u32, u32, u32)) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Item(i) if self.items.get(*i as usize).is_some_and(|l| (l.page, l.info.index, l.info.num) == key)))
    }
}

impl App {
    /// Is the list on the screen?
    pub(super) fn annots_shown(&self) -> bool {
        self.view_x > 0 && self.doc.is_some() && self.active_tab() == Tab::Annots
    }

    pub(super) fn row_top(&self, i: usize) -> i64 {
        self.al.tops.get(i).map_or(self.px(self.al.total), |&t| self.px(t))
    }

    fn row_h(&self, i: usize) -> i64 {
        self.row_top(i + 1) - self.row_top(i)
    }

    /// The rows that show anything between `y0` and `y1` (pixels from the top of the list).
    fn rows_between(&self, y0: i64, y1: i64) -> std::ops::Range<usize> {
        let to96 = |y: i64| y.saturating_mul(96) / i64::from(self.dpi).max(1);
        let (a, b) = (to96(y0), to96(y1));
        let start = self.al.tops.partition_point(|&t| t <= a).saturating_sub(1);
        let end = self.al.tops.partition_point(|&t| t < b).max(start);
        start..end
    }

    fn annots_total(&self) -> i64 {
        self.px(self.al.total)
    }

    pub(super) fn clamp_annots_scroll(&mut self) {
        let most = (self.annots_total() - self.list_h()).max(0);
        self.al.scroll = self.al.scroll.clamp(0, most);
    }

    // --- asking for the list ---------------------------------------------------------------------------------------------

    /// The file has changed (or it was opened): the list is out of date; it is asked for again if it is in view.
    pub(super) fn annots_stale(&mut self) {
        self.al.stale = true;
        self.ask_for_annot_list();
    }

    /// Ask the engine for the list if it is in view and out of date.
    pub(super) fn ask_for_annot_list(&mut self) {
        if !self.al.stale || !self.annots_shown() || self.bench.is_some() {
            return;
        }
        if let Some(old) = self.al.asked.take() {
            self.engine.cancel(old);
        }
        let id = self.new_id();
        self.engine.annot_list(self.doc_id, id);
        self.al.asked = Some(id);
        self.al.stale = false;
    }

    /// The pages are not the pages they were: the annotation chosen in the list is not the one it was.
    pub(super) fn forget_listed_selection(&mut self) {
        self.al.selected = None;
    }

    /// Forget everything about the file that is gone.
    pub(super) fn reset_annot_list(&mut self) {
        if let Some(old) = self.al.asked.take() {
            self.engine.cancel(old);
        }
        self.al = AnnotList::new();
    }

    pub(super) fn on_annot_list(&mut self, id: u64, items: Vec<ListedAnnot>, truncated: bool) -> Vec<Action> {
        if self.al.asked != Some(id) {
            return Vec::new();
        }
        self.al.asked = None;
        let cut = truncated && !self.al.truncated;
        self.al.fill(items, truncated);
        self.clamp_annots_scroll();
        let mut actions = vec![Action::Invalidate];
        if cut {
            actions.extend(self.say(t(self.lang, Msg::NoteListCut).to_string()));
        }
        actions
    }

    // --- the pointer and the keys ----------------------------------------------------------------------------------------

    /// The row of an annotation (not a page's heading) at this height of the pages' area.
    pub(super) fn annot_row_at(&self, y: i64) -> Option<usize> {
        let at = self.al.scroll + y - self.tabs_h();
        let row = self.rows_between(at, at + 1).start;
        (at >= 0 && at < self.annots_total() && matches!(self.al.rows.get(row), Some(Row::Item(_)))).then_some(row)
    }

    pub(super) fn over_annots(&self, x: i64, y: i64) -> bool {
        self.annots_shown() && x >= 0 && x < self.list_w() && y >= 0 && y < self.view_h
    }

    pub(super) fn annots_wheel(&mut self, delta: i32) -> Vec<Action> {
        self.al.scroll -= i64::from(delta) * self.px(WHEEL_STEP) / 120;
        self.clamp_annots_scroll();
        vec![Action::Invalidate]
    }

    /// A press in the list: on the bar at its side, or on a row.
    pub(super) fn annots_press(&mut self, x: i64, y: i64) -> Vec<Action> {
        self.thumb_focus = false;
        self.annots_focus = true;
        if x >= self.list_w() - self.px(GUTTER) && self.annots_total() > self.list_h() {
            self.al.dragging = true;
            return self.annots_drag_to(y);
        }
        let at = self.al.scroll + y - self.tabs_h();
        let row = self.rows_between(at, at + 1).start;
        match self.al.rows.get(row) {
            Some(Row::Item(i)) if at >= 0 && at < self.annots_total() => self.select_listed(*i as usize),
            _ => Vec::new(),
        }
    }

    /// The bar at the side of the list is dragged, the pointer now at `y`.
    pub(super) fn annots_drag_to(&mut self, y: i64) -> Vec<Action> {
        self.al.scroll = thumbs::scroll_at_bar(y - self.tabs_h(), self.list_h(), self.annots_total());
        self.clamp_annots_scroll();
        vec![Action::Invalidate]
    }

    pub(super) fn annots_drag_end(&mut self) -> bool {
        std::mem::take(&mut self.al.dragging)
    }

    pub(super) fn annots_dragging(&self) -> bool {
        self.al.dragging
    }

    /// An annotation was chosen in the list: the page is shown at it, and it is picked as soon as its page's list is here.
    fn select_listed(&mut self, i: usize) -> Vec<Action> {
        let Some(l) = self.al.items.get(i) else { return Vec::new() };
        let (page, key, rect) = (l.page, (l.page, l.info.index, l.info.num), l.info.rect);
        self.al.selected = Some(key);
        self.edit.ae.want_pick = Some(key);
        self.edit.picked = None;
        self.menus_dirty = true;
        let mut actions = self.reveal(page, rect);
        self.resolve_want_pick(page);
        actions.push(Action::Invalidate);
        actions
    }

    /// An arrow key, Home, End, Page Up or Page Down in the list: the annotation before or after, or one a screenful away.
    pub(super) fn annots_key(&mut self, key: ListKey) -> Vec<Action> {
        let items: Vec<usize> = self.al.rows.iter().enumerate().filter_map(|(r, row)| matches!(row, Row::Item(_)).then_some(r)).collect();
        if items.is_empty() {
            return Vec::new();
        }
        let now = self.al.selected.and_then(|k| self.al.item_row(k)).and_then(|r| items.iter().position(|&x| x == r));
        let screen = ((self.list_h() / self.px(ITEM_H).max(1)).max(1)) as usize;
        let last = items.len() - 1;
        let to = match (key, now) {
            (ListKey::Home, _) => 0,
            (ListKey::End, _) => last,
            (ListKey::Up | ListKey::PageUp, None) => last,
            (ListKey::Down | ListKey::PageDown, None) => 0,
            (ListKey::Up, Some(n)) => n.saturating_sub(1),
            (ListKey::Down, Some(n)) => (n + 1).min(last),
            (ListKey::PageUp, Some(n)) => n.saturating_sub(screen),
            (ListKey::PageDown, Some(n)) => (n + screen).min(last),
        };
        let Some(&row) = items.get(to) else { return Vec::new() };
        let Some(Row::Item(i)) = self.al.rows.get(row).copied() else { return Vec::new() };
        // The row comes into view.
        let (top, bottom) = (self.row_top(row), self.row_top(row + 1));
        let list_h = self.list_h();
        if top < self.al.scroll {
            self.al.scroll = top;
        } else if bottom > self.al.scroll + list_h {
            self.al.scroll = bottom - list_h;
        }
        self.clamp_annots_scroll();
        self.select_listed(i as usize)
    }

    /// Delete in the list: the annotation chosen is taken out.
    pub(super) fn annots_delete(&mut self) -> Vec<Action> {
        let Some(key) = self.al.selected else { return Vec::new() };
        let Some(Row::Item(i)) = self.al.item_row(key).and_then(|r| self.al.rows.get(r)).copied() else { return Vec::new() };
        let Some(l) = self.al.items.get(i as usize) else { return Vec::new() };
        if !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        if l.info.locked() {
            return self.say(t(self.lang, Msg::NoteLocked).to_string());
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let (page, index, num) = key;
        let id = self.new_id();
        self.engine.delete_annotation(self.doc_id, id, page, index, num);
        self.edit.asked.insert(id, Asked::Remove);
        self.al.selected = None;
        Vec::new()
    }

    // --- drawing ---------------------------------------------------------------------------------------------------------

    /// The kind of annotation as the Comment menu names its tool.
    pub(super) fn kind_name(&self, subtype: &str) -> String {
        let m = match subtype {
            "Highlight" => Msg::ToolHighlight,
            "Underline" => Msg::ToolUnderline,
            "StrikeOut" => Msg::ToolStrike,
            "Squiggly" => Msg::ToolSquiggly,
            "Ink" => Msg::ToolInk,
            "Text" => Msg::ToolNote,
            "FreeText" => Msg::ToolTextBox,
            "Stamp" => Msg::ToolStamp,
            "Square" => Msg::ToolRect,
            "Circle" => Msg::ToolEllipse,
            "Line" => Msg::ToolLine,
            "Redact" => Msg::RdKind,
            other => return other.to_string(),
        };
        plain_label(t(self.lang, m)).0
    }

    /// The picture (a letter of the icon font) of a kind of annotation.
    fn kind_glyph(subtype: &str) -> char {
        match subtype {
            "Highlight" => '\u{E7E6}',
            "Underline" => '\u{E8DC}',
            "StrikeOut" => '\u{EDE0}',
            "Squiggly" => '\u{EDC6}',
            "Ink" => '\u{EE56}',
            "Text" => '\u{E70B}',
            "FreeText" => '\u{E8AC}',
            "Stamp" => '\u{E8EC}',
            "Square" => '\u{E739}',
            "Circle" => '\u{EA3A}',
            "Line" => '\u{E72A}',
            "Redact" => '\u{E8A8}',
            _ => '\u{E90A}',
        }
    }

    /// The list: a small picture of the kind, the title (the words of the annotation, or its kind when it has none), and under it who made it and when, in the
    /// pale ink. A row chosen is on a tint of the accent, with round corners; so is the one under the pointer, faintly.
    pub(super) fn draw_annot_list(&self, p: &mut dyn Painter, width: i64, tabs: i64, bg: Rgb) {
        let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w), h: clamp32(h) };
        let list_h = self.list_h();
        p.fill_rect(rect(0, tabs, width, list_h), bg);
        if self.al.rows.is_empty() {
            if self.al.loaded {
                let s = t(self.lang, Msg::AnnotsEmpty);
                let w = i64::from(p.text_width(s));
                p.text(clamp32((width - w).max(self.px(8)) / 2), clamp32(tabs + self.px(40)), s, self.pal.disabled_fg);
            }
            return;
        }
        let picked = self.edit.picked.and_then(|(page, _)| self.edit.picked_info().map(|a| (page, a.index, a.num)));
        let (pad, inset, radius) = (self.px(PAD), self.px(6), clamp32(self.px(8)));
        let line = i64::from(p.line_height());
        for r in self.rows_between(self.al.scroll, self.al.scroll + list_h) {
            let (top, h) = (tabs + self.row_top(r) - self.al.scroll, self.row_h(r));
            match self.al.rows.get(r).copied() {
                Some(Row::Page(page)) => {
                    let s = t(self.lang, Msg::ListPage).replace("{n}", &(page + 1).to_string());
                    p.text(clamp32(inset + pad), clamp32(top + (h - line) / 2), &s, self.pal.dim_fg);
                }
                Some(Row::Item(i)) => {
                    let Some(l) = self.al.items.get(i as usize) else { continue };
                    let key = (l.page, l.info.index, l.info.num);
                    let chosen = self.al.selected == Some(key) || picked == Some(key);
                    let frame = rect(inset, top + self.px(2), width - 2 * inset, h - self.px(4));
                    if chosen {
                        p.fill_shape(frame, [radius; 4], self.pal.accent_tint, 255);
                    } else if self.side_hot == Some(r) {
                        p.fill_shape(frame, [radius; 4], self.pal.status_fg, self.pal.hover_alpha);
                    }
                    let kind = self.kind_name(&l.info.subtype);
                    let ink = if chosen { self.pal.accent_on_tint } else { self.pal.dim_fg };
                    let icon = rect(inset + pad, top, self.px(24), h);
                    p.glyph(icon, Self::kind_glyph(&l.info.subtype), if l.info.locked() { self.pal.disabled_fg } else if chosen { self.pal.accent_on_tint } else { self.pal.accent });
                    let (tx, room) = (inset + pad + self.px(32), width - inset - pad - self.px(32) - self.px(BAR_W + BAR_EDGE) - pad);
                    // (A kind with no words is its own title; who and when is left out where there is nothing to say.)
                    let title = if l.info.contents.is_empty() { kind.clone() } else { l.info.contents.clone() };
                    let by: Vec<&str> = [l.info.author.as_str(), l.info.modified.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
                    let second = if by.is_empty() { if l.info.contents.is_empty() { String::new() } else { kind } } else { by.join(" \u{B7} ") };
                    let title = fit(p, &title, room);
                    let ty = if second.is_empty() { top + (h - line) / 2 } else { top + (h - 2 * line - self.px(2)) / 2 };
                    p.text(clamp32(tx), clamp32(ty), &title, if chosen { self.pal.accent_on_tint } else if l.info.locked() { self.pal.dim_fg } else { self.pal.number_fg });
                    if !second.is_empty() {
                        let second = fit(p, &second, room);
                        p.text(clamp32(tx), clamp32(ty + line + self.px(2)), &second, ink);
                    }
                }
                None => {}
            }
        }
        if let Some((bar_top, bar_len)) = thumbs::bar(self.al.scroll, list_h, self.annots_total()) {
            p.fill_rect(rect(width - self.px(BAR_W + BAR_EDGE), tabs + bar_top, self.px(BAR_W), bar_len), self.pal.bar_color);
        }
    }
}

#[cfg(test)]
impl AnnotList {
    pub(super) fn loaded_rows(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The row of the annotation at place `index` of the list of `page`.
    pub(super) fn row_of(&self, (page, index): (u32, u32)) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Item(i) if self.items.get(*i as usize).is_some_and(|l| l.page == page && l.info.index == index)))
    }
}
