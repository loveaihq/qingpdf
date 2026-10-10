//! The reader: what is open, where the view is, what has been drawn and what is asked of the engine. It hears
//! [`Event`]s and answers with [`Action`]s; it never calls Windows, and it never waits for the engine.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use qingpdf_core::view::{Engine, Event as EngineEvent, OpenFailure, Opened, RenderRequest, RenderStatus, Rendered, TOTAL_BYTES};

use crate::bench::Bench;
use crate::cache::{BitmapCache, Entry, Focus, Key};
use crate::i18n::{self, Lang, Msg, t};
use crate::layout::Layout;
use crate::sched::{self, Scene, Want};
use crate::tiles;
use crate::ui::{Action, Bar, Event, Handler, Menu, MenuEntry, Painter, Rect, ScrollCode, ScrollInfo, vk};
use crate::zoom::{self, ZoomMode};

// Menu commands.
pub const CMD_OPEN: u32 = 1;
pub const CMD_EXIT: u32 = 2;
pub const CMD_ZOOM_IN: u32 = 10;
pub const CMD_ZOOM_OUT: u32 = 11;
pub const CMD_FIT_WIDTH: u32 = 12;
pub const CMD_FIT_PAGE: u32 = 13;
pub const CMD_ACTUAL: u32 = 14;
pub const CMD_ROTATE: u32 = 15;
pub const CMD_GOTO: u32 = 16;

// Questions asked of the user.
const TAG_PASSWORD: u32 = 1;
const TAG_GOTO: u32 = 2;

/// The timer that says the view has stopped moving.
const TIMER_SETTLE: u32 = 1;
const SETTLE_MS: u32 = 30;

// Sizes at 96 dpi.
const GAP: i64 = 10;
const MARGIN: i64 = 12;
const STATUS_HEIGHT: i64 = 26;
const WHEEL_STEP: i64 = 96;
const LINE_STEP: i64 = 48;

// Colours.
const BACKGROUND: u32 = 0xC8C8C8;
const PAPER: u32 = 0xFFFFFF;
const BORDER: u32 = 0x8C8C8C;
const STATUS_BG: u32 = 0xF0F0F0;
const STATUS_FG: u32 = 0x202020;
const NOTICE_BG: u32 = 0xFFF1B8;
const NOTICE_FG: u32 = 0x6B4E00;
const HINT_FG: u32 = 0x505050;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Notice {
    Incomplete,
    Failed,
}

/// The document that is open.
struct Doc {
    layout: Layout,
    max_tile_pixels: u32,
}

pub struct App {
    lang: Lang,
    engine: Engine,
    /// The most pixels the window can show (its screen's work area), for sizing the bitmaps' share of the memory.
    max_view_pixels: u64,
    /// The scale the pieces in view are drawn at: the zoom's own, or a smaller one when they would not fit the cache.
    draw_scale: u64,
    view_w: i64,
    view_h: i64,
    dpi: u32,
    doc_id: u64,
    path: Option<PathBuf>,
    opening: bool,
    doc: Option<Doc>,
    mode: ZoomMode,
    zoom: u32,
    /// Turns of the view in degrees, added to each page's own.
    rotation: u16,
    scroll_x: i64,
    scroll_y: i64,
    /// Scrolling or zooming goes on: nothing sharp is asked for until it stops.
    moving: bool,
    cache: BitmapCache,
    /// The requests the engine has: by the piece, and by the request.
    in_flight: HashMap<Key, u64>,
    asked: HashMap<u64, Want>,
    /// Pieces the engine could not draw: not asked for again.
    failed: HashSet<Key>,
    notices: HashMap<u32, Notice>,
    next_id: u64,
    wrong_password: bool,
    /// What the scroll bars show now, so that they are set only when it changes.
    bars: (Option<ScrollInfo>, Option<ScrollInfo>),
    /// Whether the last picture drawn had every piece of the pages in view at the zoom they are seen at.
    sharp_on_screen: bool,
    /// A file to open as soon as the window has its size (from the command line).
    pending_open: Option<PathBuf>,
    /// Wakes the window's loop (from any thread).
    wake: Arc<dyn Fn() + Send + Sync>,
    first_bitmap: Option<Instant>,
    last_sharp: Option<Instant>,
    pub(crate) bench: Option<Bench>,
    pub(crate) started: Instant,
}

impl App {
    pub fn new(lang: Lang, wake: Arc<dyn Fn() + Send + Sync>, started: Instant, max_view_pixels: u64) -> App {
        let for_engine = wake.clone();
        App {
            lang,
            engine: Engine::start(Box::new(move || for_engine())),
            max_view_pixels,
            draw_scale: sched::scale_key(zoom::ACTUAL_SIZE, 96),
            view_w: 0,
            view_h: 0,
            dpi: 96,
            doc_id: 0,
            path: None,
            opening: false,
            doc: None,
            mode: ZoomMode::FitWidth,
            zoom: zoom::ACTUAL_SIZE,
            rotation: 0,
            scroll_x: 0,
            scroll_y: 0,
            moving: false,
            cache: BitmapCache::new(0),
            in_flight: HashMap::new(),
            asked: HashMap::new(),
            failed: HashSet::new(),
            notices: HashMap::new(),
            next_id: 1,
            wrong_password: false,
            bars: (None, None),
            sharp_on_screen: false,
            pending_open: None,
            wake,
            first_bitmap: None,
            last_sharp: None,
            bench: None,
            started,
        }
    }

    /// Open this file once the window has its size.
    pub fn open_when_ready(&mut self, path: PathBuf) {
        self.pending_open = Some(path);
    }

    pub fn menus(lang: Lang) -> Vec<Menu> {
        let item = |id, m| MenuEntry::Item { id, label: t(lang, m).to_string() };
        vec![
            Menu { title: t(lang, Msg::File).to_string(), entries: vec![item(CMD_OPEN, Msg::Open), MenuEntry::Separator, item(CMD_EXIT, Msg::Exit)] },
            Menu {
                title: t(lang, Msg::View).to_string(),
                entries: vec![
                    item(CMD_ZOOM_IN, Msg::ZoomIn),
                    item(CMD_ZOOM_OUT, Msg::ZoomOut),
                    MenuEntry::Separator,
                    item(CMD_FIT_WIDTH, Msg::FitWidth),
                    item(CMD_FIT_PAGE, Msg::FitPage),
                    item(CMD_ACTUAL, Msg::ActualSize),
                    MenuEntry::Separator,
                    item(CMD_ROTATE, Msg::Rotate),
                    item(CMD_GOTO, Msg::GoToPage),
                ],
            },
        ]
    }

    fn px(&self, at96: i64) -> i64 {
        at96 * i64::from(self.dpi) / 96
    }

    fn status_height(&self) -> i64 {
        self.px(STATUS_HEIGHT)
    }

    // --- opening ---------------------------------------------------------------------------------------------

    /// Open a file (the engine reads it; this does not wait).
    pub fn open_path(&mut self, path: &Path, password: &str) -> Vec<Action> {
        self.drop_document();
        if self.engine.start_error().is_some() {
            // No drawing thread: nothing would ever answer, so say so instead of showing "Opening..." for ever.
            self.opening = false;
            self.path = None;
            let lang = self.lang;
            return vec![Action::SetTitle(self.title()), Action::Message { title: t(lang, Msg::Error).to_string(), text: t(lang, Msg::EngineFailed).to_string() }, Action::Invalidate];
        }
        self.doc_id += 1;
        self.path = Some(path.to_path_buf());
        self.opening = true;
        let view = u64::try_from(self.view_w.max(0)).unwrap_or(0).saturating_mul(u64::try_from(self.view_h.max(0)).unwrap_or(0));
        self.engine.open_file(self.doc_id, &path.to_string_lossy(), password, TOTAL_BYTES, view.max(self.max_view_pixels));
        vec![Action::SetTitle(self.title()), Action::Invalidate]
    }

    fn drop_document(&mut self) {
        // What is being drawn for the old file is not wanted: stop it (opening another file stops it too, but the
        // file may fail to open, and the old requests are not to go on).
        for (id, _) in self.asked.drain() {
            self.engine.cancel(id);
        }
        self.doc = None;
        self.cache.clear();
        self.in_flight.clear();
        self.asked.clear();
        self.failed.clear();
        self.notices.clear();
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.moving = false;
        self.sharp_on_screen = false;
    }

    fn title(&self) -> String {
        match &self.path {
            Some(p) => format!("{} - qingpdf", p.file_name().map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())),
            None => "qingpdf".to_string(),
        }
    }

    fn on_opened(&mut self, o: Opened) -> Vec<Action> {
        if o.doc != self.doc_id {
            return Vec::new();
        }
        self.opening = false;
        let sizes = o.pages.iter().map(|p| (p.width, p.height)).collect();
        self.doc = Some(Doc { layout: Layout::new(sizes), max_tile_pixels: o.max_tile_pixels });
        self.cache = BitmapCache::new(o.bitmap_cache_bytes);
        self.mode = ZoomMode::FitWidth;
        self.rotation = 0;
        self.scroll_x = 0;
        self.scroll_y = 0;
        let mut actions = vec![Action::SetTitle(self.title())];
        self.relayout(0, 0, None);
        actions.extend(self.after_view_change(false));
        actions
    }

    fn on_open_failed(&mut self, doc: u64, failure: OpenFailure, message: &str) -> Vec<Action> {
        if doc != self.doc_id {
            return Vec::new();
        }
        self.opening = false;
        let lang = self.lang;
        let mut actions = vec![Action::Invalidate];
        match failure {
            OpenFailure::NeedsPassword | OpenFailure::WrongPassword => {
                let wrong = failure == OpenFailure::WrongPassword;
                self.wrong_password = wrong;
                let prompt = t(lang, if wrong { Msg::PasswordWrongPrompt } else { Msg::PasswordPrompt });
                actions.push(Action::AskText {
                    tag: TAG_PASSWORD,
                    title: t(lang, Msg::PasswordTitle).to_string(),
                    prompt: prompt.to_string(),
                    secret: true,
                    ok: t(lang, Msg::Ok).to_string(),
                    cancel: t(lang, Msg::Cancel).to_string(),
                });
            }
            OpenFailure::TooLarge => actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: t(lang, Msg::TooLarge).to_string() }),
            OpenFailure::Unreadable => {
                actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::CannotRead)) });
            }
            OpenFailure::Damaged => actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: t(lang, Msg::NotPdf).to_string() }),
        }
        if failure != OpenFailure::NeedsPassword && failure != OpenFailure::WrongPassword {
            self.path = None;
            actions.push(Action::SetTitle(self.title()));
        }
        actions
    }

    // --- the view --------------------------------------------------------------------------------------------

    /// Lay the pages out again for the zoom, the rotation and the size of the window. The place that is `anchor_y`
    /// pixels down the window (and `anchor_x` across) stays where it is. `mode`: set the zoom from a fit first.
    fn relayout(&mut self, anchor_x: i64, anchor_y: i64, zoom_to: Option<(ZoomMode, u32)>) {
        let (dpi, rotation, gap, margin) = (self.dpi, self.rotation, self.px(GAP), self.px(MARGIN));
        let (view_w, view_h) = (self.view_w, self.view_h);
        let Some(doc) = self.doc.as_mut() else { return };
        let l = &mut doc.layout;
        let (page, fraction) = l.locate(self.scroll_y + anchor_y);
        let old_w = l.content_width().max(1);
        let x_fraction = (self.scroll_x + anchor_x) as f64 / old_w as f64;
        // The rotation first: the fit works on the pages as they are shown.
        l.set(l.scale(), rotation, gap, margin);
        if let Some((mode, z)) = zoom_to {
            self.mode = mode;
            self.zoom = zoom::clamp(z);
        }
        if self.mode != ZoomMode::Custom {
            self.zoom = fit_zoom(l, self.mode, page, view_w, view_h, margin, dpi);
        }
        l.set(zoom::scale(self.zoom, dpi), rotation, gap, margin);
        self.scroll_y = l.y_of(page, fraction) - anchor_y;
        self.scroll_x = (x_fraction * l.content_width() as f64).round() as i64 - anchor_x;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let Some(doc) = &self.doc else { return };
        let (mx, my) = doc.layout.max_scroll(self.view_w, self.view_h);
        self.scroll_x = self.scroll_x.clamp(0, mx);
        self.scroll_y = self.scroll_y.clamp(0, my);
    }

    /// The pages in view (first, last).
    fn visible(&self) -> Option<(u32, u32)> {
        let doc = self.doc.as_ref()?;
        let r = doc.layout.visible(self.scroll_y, self.scroll_y + self.view_h);
        (!r.is_empty()).then(|| (r.start as u32, (r.end - 1) as u32))
    }

    fn focus(&self) -> Focus {
        let (first, last) = self.visible().unwrap_or((0, 0));
        Focus { first, last, rotation: self.rotation, scale: self.draw_scale }
    }

    /// The page at the middle of the window (1-based for people).
    pub fn current_page(&self) -> usize {
        self.doc.as_ref().map_or(0, |d| d.layout.page_at(self.scroll_y + self.view_h / 2))
    }

    pub fn page_count(&self) -> usize {
        self.doc.as_ref().map_or(0, |d| d.layout.len())
    }

    /// After the view has moved or changed: bars, what to ask the engine, and a new picture. `moving`: the view is
    /// still being scrolled (a timer says when it has stopped).
    fn after_view_change(&mut self, moving: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        self.moving = moving;
        if moving {
            actions.push(Action::SetTimer { id: TIMER_SETTLE, ms: SETTLE_MS });
        } else {
            actions.push(Action::KillTimer(TIMER_SETTLE));
        }
        self.sync_bars(&mut actions);
        self.schedule();
        actions.push(Action::Invalidate);
        actions
    }

    fn sync_bars(&mut self, actions: &mut Vec<Action>) {
        let Some(doc) = &self.doc else {
            let none = ScrollInfo { min: 0, max: 0, page: 0, pos: 0 };
            if self.bars != (Some(none), None) {
                self.bars = (Some(none), None);
                actions.push(Action::SetScroll { bar: Bar::Vertical, info: none });
                actions.push(Action::SetScroll { bar: Bar::Horizontal, info: none });
            }
            return;
        };
        let l = &doc.layout;
        // The bar counts in whole numbers up to 2^31: a very long document is counted in bigger steps.
        let step = (l.content_height() / 2_000_000_000 + 1).max(1);
        let v = ScrollInfo {
            min: 0,
            max: (l.content_height() / step - 1).max(0) as i32,
            page: (self.view_h / step).max(0) as u32,
            pos: (self.scroll_y / step) as i32,
        };
        let h = ScrollInfo { min: 0, max: (l.content_width() - 1).clamp(0, i64::from(i32::MAX)) as i32, page: self.view_w.max(0) as u32, pos: self.scroll_x.clamp(0, i64::from(i32::MAX)) as i32 };
        if self.bars.0 != Some(v) {
            self.bars.0 = Some(v);
            actions.push(Action::SetScroll { bar: Bar::Vertical, info: v });
        }
        if self.bars.1 != Some(h) {
            self.bars.1 = Some(h);
            actions.push(Action::SetScroll { bar: Bar::Horizontal, info: h });
        }
    }

    /// What the window shows, for the scheduler.
    fn scene(&self) -> Option<Scene<'_>> {
        let doc = self.doc.as_ref()?;
        Some(Scene {
            layout: &doc.layout,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
            view_w: self.view_w,
            view_h: self.view_h,
            zoom: self.zoom,
            dpi: self.dpi,
            moving: self.moving,
            max_tile_pixels: doc.max_tile_pixels,
            budget: self.cache.budget(),
        })
    }

    /// Ask the engine for what is missing and take back what is not wanted any more.
    fn schedule(&mut self) {
        let Some(scene) = self.scene() else { return };
        let scale = sched::draw_scale(&scene);
        let mut wanted = sched::wanted(&scene, &self.cache);
        self.draw_scale = scale;
        wanted.retain(|w| !self.failed.contains(&w.key));
        let plan = sched::plan(&wanted, &self.cache, &self.in_flight);
        for id in plan.cancel {
            self.engine.cancel(id);
            if let Some(w) = self.asked.remove(&id) {
                self.in_flight.remove(&w.key);
            }
        }
        for w in plan.issue {
            let id = self.next_id;
            self.next_id += 1;
            self.engine.render(RenderRequest {
                doc: self.doc_id,
                id,
                page: w.key.page,
                dpi: sched::dpi_of(w.key.scale),
                rotation: w.key.rotation,
                x: w.x.clamp(0, i64::from(u32::MAX)) as u32,
                y: w.y.clamp(0, i64::from(u32::MAX)) as u32,
                width: w.w.clamp(0, i64::from(u32::MAX)) as u32,
                height: w.h.clamp(0, i64::from(u32::MAX)) as u32,
                priority: w.priority,
            });
            self.in_flight.insert(w.key, id);
            self.asked.insert(id, w);
        }
    }

    // --- the engine's answers --------------------------------------------------------------------------------

    fn pump_engine(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut redraw = false;
        while let Some(event) = self.engine.poll() {
            match event {
                EngineEvent::Opened(o) => actions.extend(self.on_opened(o)),
                EngineEvent::OpenFailed { doc, failure, message } => actions.extend(self.on_open_failed(doc, failure, &message)),
                EngineEvent::Rendered(r) => redraw |= self.on_rendered(r),
            }
        }
        if redraw {
            actions.push(Action::Invalidate);
        }
        actions
    }

    /// A piece came back. `true`: it is in view, so the window needs a new picture.
    fn on_rendered(&mut self, r: Rendered) -> bool {
        if r.doc != self.doc_id {
            return false;
        }
        let Some(want) = self.asked.remove(&r.id) else { return false };
        self.in_flight.remove(&want.key);
        let page = want.key.page;
        if r.status == RenderStatus::Failed {
            self.failed.insert(want.key);
            if !want.preview {
                self.notices.insert(page, Notice::Failed);
            }
            return self.page_in_view(page);
        }
        if r.status == RenderStatus::Incomplete && !want.preview {
            self.notices.insert(page, Notice::Incomplete);
        }
        let entry = Entry {
            key: want.key,
            width: r.width,
            height: r.height,
            bgra: r.bgra,
            preview: want.preview,
            x: want.x,
            y: want.y,
            page_w: want.page_w,
            page_h: want.page_h,
        };
        let focus = self.focus();
        self.cache.insert(entry, &focus);
        // The page is whole at this zoom now: what was drawn for another zoom can go.
        if !want.preview && self.page_is_sharp(page) {
            self.cache.drop_stale(page, focus.rotation, focus.scale);
        }
        self.page_in_view(page)
    }

    fn page_in_view(&self, page: u32) -> bool {
        self.visible().is_some_and(|(a, b)| (a..=b).contains(&page))
    }

    /// The size of a page in pixels at the scale the pieces in view are drawn at.
    fn page_px_at_draw_scale(&self, page: usize) -> (i64, i64) {
        let Some(doc) = &self.doc else { return (0, 0) };
        if self.draw_scale == sched::scale_key(self.zoom, self.dpi) {
            return doc.layout.size_px(page);
        }
        doc.layout.size_pt(page).map_or((0, 0), |(w, h)| sched::page_px(w, h, self.draw_scale))
    }

    /// Does the cache hold every tile of the page at the scale it is drawn at?
    fn page_is_sharp(&self, page: u32) -> bool {
        let Some(doc) = &self.doc else { return false };
        let (pw, ph) = self.page_px_at_draw_scale(page as usize);
        let have = self.cache.page(page).filter(|e| !e.preview && e.key.rotation == self.rotation && e.key.scale == self.draw_scale).count() as u64;
        have >= tiles::count(pw, ph, doc.max_tile_pixels)
    }

    /// Is everything in view drawn sharp (the pieces in view, at the scale they are drawn at)? Used by the speed test.
    pub fn visible_ready(&self) -> bool {
        let Some(scene) = self.scene() else { return false };
        let visible = scene.layout.visible(self.scroll_y, self.scroll_y + self.view_h);
        !visible.is_empty() && sched::in_view(&scene).0.iter().all(|w| self.cache.contains(&w.key) || self.failed.contains(&w.key))
    }

    /// Is the engine asked for nothing (everything wanted has come in)? Used by the speed test.
    pub fn is_idle(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Are the pieces in view drawn smaller than the zoom, and stretched (they would not fit the cache)?
    pub fn reduced(&self) -> bool {
        self.draw_scale != sched::scale_key(self.zoom, self.dpi)
    }

    /// When the first picture with any page bitmap in it was drawn.
    pub fn first_bitmap_painted(&self) -> Option<Instant> {
        self.first_bitmap
    }

    /// When the last picture was drawn that had every piece in view sharp (none since `clear_last_sharp_paint`).
    pub fn last_sharp_paint(&self) -> Option<Instant> {
        self.last_sharp
    }

    pub fn clear_last_sharp_paint(&mut self) {
        self.last_sharp = None;
    }

    pub fn has_document(&self) -> bool {
        self.doc.is_some()
    }

    /// For the speed test: let go of every bitmap and every request.
    pub fn forget_bitmaps(&mut self) {
        for (id, _) in self.asked.drain() {
            self.engine.cancel(id);
        }
        self.in_flight.clear();
        self.cache.clear();
        self.notices.clear();
        self.failed.clear();
    }

    // --- moving about ----------------------------------------------------------------------------------------

    /// Scroll to a place (document pixels), clamped; `moving`: more is coming.
    pub fn scroll_to(&mut self, x: i64, y: i64, moving: bool) -> Vec<Action> {
        self.scroll_x = x;
        self.scroll_y = y;
        self.clamp_scroll();
        self.after_view_change(moving)
    }

    pub fn go_to_page(&mut self, page: usize) -> Vec<Action> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let page = page.min(doc.layout.len().saturating_sub(1));
        let y = doc.layout.scroll_to_page(page, self.px(MARGIN) / 2);
        self.scroll_to(self.scroll_x, y, false)
    }

    fn set_zoom(&mut self, mode: ZoomMode, zoom_value: u32, anchor: Option<(i64, i64)>) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let (ax, ay) = anchor.unwrap_or((self.view_w / 2, self.view_h / 2));
        self.relayout(ax, ay, Some((mode, zoom_value)));
        self.after_view_change(true)
    }

    fn rotate(&mut self, quarter_turns: u16) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        self.rotation = (self.rotation + 90 * quarter_turns) % 360;
        // The page in the middle of the window stays there.
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        self.relayout(ax, ay, None);
        self.after_view_change(false)
    }

    fn scroll_by(&mut self, dx: i64, dy: i64, moving: bool) -> Vec<Action> {
        self.scroll_to(self.scroll_x + dx, self.scroll_y + dy, moving)
    }

    fn command(&mut self, cmd: u32) -> Vec<Action> {
        match cmd {
            CMD_OPEN => {
                let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string()), (t(self.lang, Msg::FilterAll).to_string(), "*.*".to_string())];
                vec![Action::PickFile { title: t(self.lang, Msg::OpenTitle).to_string(), filters }]
            }
            CMD_EXIT => vec![Action::Quit],
            CMD_ZOOM_IN => self.set_zoom(ZoomMode::Custom, zoom::step_in(self.zoom), None),
            CMD_ZOOM_OUT => self.set_zoom(ZoomMode::Custom, zoom::step_out(self.zoom), None),
            CMD_FIT_WIDTH => self.set_zoom(ZoomMode::FitWidth, self.zoom, None),
            CMD_FIT_PAGE => self.set_zoom(ZoomMode::FitPage, self.zoom, None),
            CMD_ACTUAL => self.set_zoom(ZoomMode::Custom, zoom::ACTUAL_SIZE, None),
            CMD_ROTATE => self.rotate(1),
            CMD_GOTO if self.doc.is_some() => {
                let title = t(self.lang, Msg::GoToTitle).to_string();
                let prompt = format!("{} (1-{})", t(self.lang, Msg::GoToTitle), self.page_count());
                let (ok, cancel) = (t(self.lang, Msg::Ok).to_string(), t(self.lang, Msg::Cancel).to_string());
                vec![Action::AskText { tag: TAG_GOTO, title, prompt, secret: false, ok, cancel }]
            }
            _ => Vec::new(),
        }
    }

    fn key(&mut self, key: u32, ctrl: bool, shift: bool) -> Vec<Action> {
        let page_step = (self.view_h * 9 / 10).max(1);
        let line = self.px(LINE_STEP);
        if ctrl {
            return match key {
                vk::O => self.command(CMD_OPEN),
                vk::G => self.command(CMD_GOTO),
                vk::R => {
                    if shift {
                        self.rotate(3)
                    } else {
                        self.rotate(1)
                    }
                }
                vk::OEM_PLUS | vk::ADD => self.command(CMD_ZOOM_IN),
                vk::OEM_MINUS | vk::SUBTRACT => self.command(CMD_ZOOM_OUT),
                vk::NUM0 | vk::NUMPAD0 => self.command(CMD_ACTUAL),
                vk::NUM1 => self.command(CMD_FIT_WIDTH),
                vk::NUM2 => self.command(CMD_FIT_PAGE),
                _ => Vec::new(),
            };
        }
        match key {
            vk::NEXT => self.scroll_by(0, page_step, false),
            vk::PRIOR => self.scroll_by(0, -page_step, false),
            vk::SPACE => self.scroll_by(0, if shift { -page_step } else { page_step }, false),
            vk::DOWN => self.scroll_by(0, line, false),
            vk::UP => self.scroll_by(0, -line, false),
            vk::RIGHT => self.scroll_by(line, 0, false),
            vk::LEFT => self.scroll_by(-line, 0, false),
            vk::HOME => self.scroll_to(self.scroll_x, 0, false),
            vk::END => self.scroll_to(self.scroll_x, i64::MAX / 4, false),
            _ => Vec::new(),
        }
    }

    fn scrolled_by_bar(&mut self, bar: Bar, code: ScrollCode) -> Vec<Action> {
        let line = self.px(LINE_STEP);
        let page_step = (self.view_h * 9 / 10).max(1);
        let Some(doc) = &self.doc else { return Vec::new() };
        let step = (doc.layout.content_height() / 2_000_000_000 + 1).max(1);
        match (bar, code) {
            (Bar::Vertical, ScrollCode::LineUp) => self.scroll_by(0, -line, false),
            (Bar::Vertical, ScrollCode::LineDown) => self.scroll_by(0, line, false),
            (Bar::Vertical, ScrollCode::PageUp) => self.scroll_by(0, -page_step, false),
            (Bar::Vertical, ScrollCode::PageDown) => self.scroll_by(0, page_step, false),
            (Bar::Vertical, ScrollCode::Top) => self.scroll_to(self.scroll_x, 0, false),
            (Bar::Vertical, ScrollCode::Bottom) => self.scroll_to(self.scroll_x, i64::MAX / 4, false),
            (Bar::Vertical, ScrollCode::Thumb(p)) => self.scroll_to(self.scroll_x, i64::from(p) * step, true),
            (Bar::Horizontal, ScrollCode::LineUp) => self.scroll_by(-line, 0, false),
            (Bar::Horizontal, ScrollCode::LineDown) => self.scroll_by(line, 0, false),
            (Bar::Horizontal, ScrollCode::PageUp) => self.scroll_by(-self.view_w * 9 / 10, 0, false),
            (Bar::Horizontal, ScrollCode::PageDown) => self.scroll_by(self.view_w * 9 / 10, 0, false),
            (Bar::Horizontal, ScrollCode::Top) => self.scroll_to(0, self.scroll_y, false),
            (Bar::Horizontal, ScrollCode::Bottom) => self.scroll_to(i64::MAX / 4, self.scroll_y, false),
            (Bar::Horizontal, ScrollCode::Thumb(p)) => self.scroll_to(i64::from(p), self.scroll_y, true),
        }
    }

    // --- drawing ---------------------------------------------------------------------------------------------

    /// Draw a page; `true` if some of its bitmaps are on it.
    fn draw_page(&self, p: &mut dyn Painter, page: usize) -> bool {
        let Some(doc) = &self.doc else { return false };
        let l = &doc.layout;
        let (pw, ph) = l.size_px(page);
        let (left, top) = (l.left(page, self.view_w, self.scroll_x), l.top(page) - self.scroll_y);
        let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w), h: clamp32(h) };
        p.fill_rect(rect(left - 1, top - 1, pw + 2, ph + 2), BORDER);
        p.fill_rect(rect(left, top, pw, ph), PAPER);
        let want = self.draw_scale;
        let mut entries: Vec<&Entry> = self.cache.page(page as u32).filter(|e| e.key.rotation == self.rotation).collect();
        let exact = entries.iter().filter(|e| !e.preview && e.key.scale == want).count() as u64;
        let (dw, dh) = self.page_px_at_draw_scale(page);
        let whole = exact >= tiles::count(dw, dh, doc.max_tile_pixels);
        if whole {
            entries.retain(|e| !e.preview && e.key.scale == want);
        }
        // Coarse first, so that the sharp pieces come over them.
        entries.sort_by_key(|e| (e.key.scale == want && !e.preview, e.key.scale, e.key.ty, e.key.tx));
        let drew = !entries.is_empty();
        for e in entries {
            // The piece is put where the same part of the page is now: one drawn at the page's own size lands on its
            // pixels, a smaller one (another zoom, a preview, a page drawn smaller to fit the cache) is stretched.
            let (sx, sy) = (pw as f64 / e.page_w.max(1) as f64, ph as f64 / e.page_h.max(1) as f64);
            let (x0, y0) = ((e.x as f64 * sx).floor() as i64, (e.y as f64 * sy).floor() as i64);
            let (x1, y1) = (((e.x + i64::from(e.width)) as f64 * sx).ceil() as i64, ((e.y + i64::from(e.height)) as f64 * sy).ceil() as i64);
            p.draw_bgra(rect(left + x0, top + y0, x1 - x0, y1 - y0), e.width as i32, e.height as i32, &e.bgra);
        }
        match self.notices.get(&(page as u32)) {
            Some(Notice::Incomplete) => {
                let strip = i64::from(p.line_height()) + 8;
                p.fill_rect(rect(left, top, pw, strip), NOTICE_BG);
                p.text(clamp32(left + 8), clamp32(top + 4), t(self.lang, Msg::NoticeIncomplete), NOTICE_FG);
            }
            Some(Notice::Failed) => {
                let s = t(self.lang, Msg::NoticeFailed);
                let w = i64::from(p.text_width(s));
                p.text(clamp32(left + (pw - w) / 2), clamp32(top + 24), s, NOTICE_FG);
            }
            None => {}
        }
        drew
    }
}

fn clamp32(v: i64) -> i32 {
    v.clamp(-100_000_000, 100_000_000) as i32
}

/// The zoom that fits the pages into the window: across, the widest page (so that no page is wider than the window
/// and the window never needs a horizontal bar; the narrower ones have room at the sides); for a whole page, the
/// height of page `page` too.
fn fit_zoom(l: &Layout, mode: ZoomMode, page: usize, view_w: i64, view_h: i64, margin: i64, dpi: u32) -> u32 {
    let Some((_, h_pt)) = l.size_pt(page) else { return zoom::ACTUAL_SIZE };
    let across = zoom::fit((view_w - 2 * margin) as f64, l.widest_pt(), dpi);
    match mode {
        ZoomMode::FitWidth => across,
        ZoomMode::FitPage => across.min(zoom::fit((view_h - 2 * margin) as f64, h_pt, dpi)),
        ZoomMode::Custom => zoom::ACTUAL_SIZE,
    }
}

impl Handler for App {
    fn event(&mut self, event: Event) -> Vec<Action> {
        let mut actions = match event {
            Event::Size { w, h } => {
                self.view_w = i64::from(w);
                self.view_h = (i64::from(h) - self.status_height()).max(0);
                let (ax, ay) = (self.view_w / 2, self.view_h / 2);
                self.relayout(ax, ay, None);
                let mut actions = self.after_view_change(false);
                if let Some(path) = self.pending_open.take() {
                    actions.extend(self.open_path(&path, ""));
                }
                actions
            }
            Event::Dpi(dpi) => {
                self.dpi = dpi.clamp(48, 960);
                // The pages are the same size in inches, the pixels change: draw them all again.
                self.forget_bitmaps();
                let (ax, ay) = (self.view_w / 2, self.view_h / 2);
                self.relayout(ax, ay, None);
                self.after_view_change(false)
            }
            Event::Key { vk, ctrl, shift } => self.key(vk, ctrl, shift),
            Event::Wheel { delta, ctrl, shift, x, y } => {
                if ctrl {
                    let z = zoom::wheel(self.zoom, f64::from(delta) / 120.0);
                    self.set_zoom(ZoomMode::Custom, z, Some((i64::from(x), i64::from(y))))
                } else if shift {
                    self.scroll_by(-i64::from(delta) * self.px(WHEEL_STEP) / 120, 0, true)
                } else {
                    self.scroll_by(0, -i64::from(delta) * self.px(WHEEL_STEP) / 120, true)
                }
            }
            Event::HWheel { delta } => self.scroll_by(i64::from(delta) * self.px(WHEEL_STEP) / 120, 0, true),
            Event::Scroll { bar, code } => self.scrolled_by_bar(bar, code),
            Event::Command(cmd) => self.command(cmd),
            Event::Drop(files) => match files.first() {
                Some(f) => self.open_path(f, ""),
                None => Vec::new(),
            },
            Event::Wake => self.pump_engine(),
            Event::Timer(TIMER_SETTLE) => {
                self.moving = false;
                self.schedule();
                vec![Action::KillTimer(TIMER_SETTLE)]
            }
            Event::Timer(_) => Vec::new(),
            Event::FilePicked(Some(path)) => self.open_path(&path, ""),
            Event::FilePicked(None) => Vec::new(),
            Event::Text { tag: TAG_PASSWORD, text } => match (text, self.path.clone()) {
                (Some(mut pw), Some(path)) => {
                    let actions = self.open_path(&path, &pw);
                    // The password has gone to the engine; overwrite our copy.
                    let mut bytes = std::mem::take(&mut pw).into_bytes();
                    bytes.fill(0);
                    std::hint::black_box(&bytes);
                    actions
                }
                _ => {
                    self.path = None;
                    vec![Action::SetTitle(self.title()), Action::Invalidate]
                }
            },
            Event::Text { tag: TAG_GOTO, text } => match text.and_then(|s| s.trim().parse::<usize>().ok()) {
                Some(n) if n >= 1 => self.go_to_page(n - 1),
                _ => Vec::new(),
            },
            Event::Text { .. } => Vec::new(),
        };
        // Whatever the event was, the engine may have something waiting; the speed test may have its next step.
        if let Some(mut bench) = self.bench.take() {
            actions.extend(bench.step(self));
            self.bench = Some(bench);
        }
        actions
    }

    fn paint(&mut self, p: &mut dyn Painter) -> bool {
        let (full_w, full_h) = (clamp32(self.view_w), clamp32(self.view_h + self.status_height()));
        p.fill_rect(Rect { x: 0, y: 0, w: full_w, h: full_h }, BACKGROUND);
        let mut drew = false;
        match &self.doc {
            Some(doc) => {
                for page in doc.layout.visible(self.scroll_y, self.scroll_y + self.view_h) {
                    drew |= self.draw_page(p, page);
                }
            }
            None => {
                let s = t(self.lang, if self.opening { Msg::Opening } else { Msg::Hint });
                let w = p.text_width(s);
                p.text((full_w - w) / 2, (clamp32(self.view_h) - p.line_height()) / 2, s, HINT_FG);
            }
        }
        let now = Instant::now();
        if drew && self.first_bitmap.is_none() {
            self.first_bitmap = Some(now);
        }
        self.sharp_on_screen = self.visible_ready();
        if self.sharp_on_screen {
            self.last_sharp = Some(now);
        }
        if self.bench.is_some() {
            // The speed test looks at what was drawn after this picture is on the screen.
            (self.wake)();
        }
        // The status line.
        let bar = clamp32(self.status_height());
        p.fill_rect(Rect { x: 0, y: clamp32(self.view_h), w: full_w, h: bar }, STATUS_BG);
        if self.doc.is_some() {
            let y = clamp32(self.view_h) + (bar - p.line_height()) / 2;
            p.text(clamp32(self.px(10)), y, &i18n::page_of(self.lang, self.current_page() + 1, self.page_count()), STATUS_FG);
            let z = format!("{}%", (f64::from(self.zoom) / 10.0).round() as u32);
            let w = p.text_width(&z);
            p.text(full_w - w - clamp32(self.px(10)), y, &z, STATUS_FG);
        }
        // The speed test stops its clock when the page is drawn; it does not put the pictures before that on the
        // screen (Windows sometimes holds a BitBlt to the screen for half a second, whatever is in it).
        self.bench.is_none() || self.sharp_on_screen
    }
}
