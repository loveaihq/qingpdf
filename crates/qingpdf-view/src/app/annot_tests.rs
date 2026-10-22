//! Shapes, picking, moving, resizing, recolouring and typing over annotations that are on the page, and the list of annotations in the sidebar
//! (2026-10-17), through the reader's events and the real engine.

use std::time::{Duration, Instant};

use super::*;
use qingpdf_core::view::AnnotInfo;
use crate::i18n::Lang;
use crate::ui::{FieldExit, MenuEntry};

fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
    for _ in 0..1500 {
        app.event(Event::Wake);
        if until(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn pdf(objects: &[(u32, String)]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

/// The annotations of the test file (objects 10 and up): in the order of the page's list.
const SQUARE: usize = 0;
const LINE: usize = 1;
const NOTE: usize = 2;
const LOCKED: usize = 3;
const STAMP: usize = 4;
const HIGHLIGHT: usize = 5;
const TEXTBOX: usize = 6;

/// A file of two pages of 600 x 800 points; the first has the annotations above, the second one note.
fn annotated(name: &str) -> std::path::PathBuf {
    let annots = [
        "<< /Type /Annot /Subtype /Square /Rect [100 600 200 700] /C [1 0 0] /BS << /W 2 /S /S >> /F 4 /T (Ann) /M (D:20261001120000Z) /Contents (a square) >>",
        "<< /Type /Annot /Subtype /Line /Rect [300 600 400 700] /L [300 600 400 700] /C [0 0 1] /BS << /W 2 >> /F 4 >>",
        "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /F 28 /Contents (hello note) /T (Bob) /M (D:20261002120000Z) /Name /Comment >>",
        "<< /Type /Annot /Subtype /Square /Rect [450 600 550 700] /C [0 0.5 0] /F 132 /Contents (locked) >>",
        "<< /Type /Annot /Subtype /Stamp /Rect [100 300 200 400] /F 4 /AP << /N 5 0 R >> >>",
        "<< /Type /Annot /Subtype /Highlight /Rect [100 500 200 520] /C [1 1 0] /F 4 /QuadPoints [100 520 200 520 100 500 200 500] >>",
        "<< /Type /Annot /Subtype /FreeText /Rect [300 300 450 360] /F 4 /Contents (box text) /DA (/Helv 12 Tf 0 g) /Border [0 0 0] >>",
    ];
    let refs: String = (0..annots.len()).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [{refs}] >>")),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [30 0 R] >>".to_string()),
        (5, "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Length 18 >>\nstream\n0 0 1 rg 0 0 100 100 re f\nendstream".to_string()),
        (30, "<< /Type /Annot /Subtype /Text /Rect [100 100 120 120] /F 28 /Contents (second page) /T (Eve) /M (D:20261003120000Z) >>".to_string()),
    ];
    for (i, a) in annots.iter().enumerate() {
        objects.push((10 + i as u32, (*a).to_string()));
    }
    let dir = std::env::temp_dir().join(format!("qingpdf-view-annot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.pdf"));
    std::fs::write(&file, pdf(&objects)).unwrap();
    file
}

fn open(name: &str) -> App {
    let file = annotated(name);
    let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
    app.persist = false;
    app.recent_path = None;
    app.event(Event::Size { w: 1000, h: 900 });
    app.open_path(&file, "");
    pump(&mut app, "the file to open", |a| a.has_document());
    app.event(Event::Command(CMD_FIT_PAGE));
    pump(&mut app, "the annotations", |a| a.edit.annots.get(&0).is_some_and(|l| l.len() == 7));
    app
}

/// The annotation `which` of page 0 as the reader knows it.
fn info(app: &App, which: usize) -> AnnotInfo {
    app.edit.annots.get(&0).and_then(|l| l.iter().find(|a| a.index as usize == which)).cloned().expect("the annotation is listed")
}

/// A place in the window inside the annotation `which`, its middle.
fn middle(app: &App, which: usize) -> (i32, i32) {
    let a = info(app, which);
    let r = app.window_rect(0, a.rect).expect("on the window");
    (r.x + r.w / 2, r.y + r.h / 2)
}

fn down(app: &mut App, at: (i32, i32), clicks: u8) -> Vec<Action> {
    app.event(Event::MouseDown { x: at.0, y: at.1, ctrl: false, shift: false, clicks })
}

fn up(app: &mut App, at: (i32, i32)) -> Vec<Action> {
    app.event(Event::MouseUp { x: at.0, y: at.1 })
}

fn drag(app: &mut App, from: (i32, i32), to: (i32, i32), shift: bool) {
    down(app, from, 1);
    let steps = 5;
    for i in 1..=steps {
        let (x, y) = (from.0 + (to.0 - from.0) * i / steps, from.1 + (to.1 - from.1) * i / steps);
        app.event(Event::MouseMove { x, y, shift });
    }
    up(app, to);
}

/// Wait for the engine to answer the change and the page's annotations to be read again.
fn settled(app: &mut App) {
    pump(app, "the edit", |a| a.edit.asked.is_empty());
    pump(app, "the annotations again", |a| a.edit.annots.contains_key(&0) && a.edit.ae.want_pick.is_none());
}

fn click(app: &mut App, at: (i32, i32)) {
    down(app, at, 1);
    up(app, at);
}

fn menu_enabled(app: &App, cmd: u32) -> bool {
    fn find(entries: &[MenuEntry], cmd: u32) -> Option<bool> {
        entries.iter().find_map(|e| match e {
            MenuEntry::Item { id, enabled, .. } if *id == cmd => Some(*enabled),
            MenuEntry::Submenu { entries, .. } => find(entries, cmd),
            _ => None,
        })
    }
    app.menus().iter().find_map(|m| find(&m.entries, cmd)).expect("the command is in a menu")
}

fn rect_of(app: &App, which: usize) -> [f32; 4] {
    info(app, which).rect
}

fn near(a: [f32; 4], b: [f32; 4], slack: f32) {
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= slack), "{a:?} is not {b:?}");
}

#[test]
fn shapes_are_dragged_out_square_and_round_with_shift_and_a_click_draws_nothing() {
    let mut app = open("shapes");
    let before = app.edit.annots.get(&0).unwrap().len();
    // The tools are in the Comment menu and the tool in use is checked.
    for cmd in [CMD_TOOL_RECT, CMD_TOOL_ELLIPSE, CMD_TOOL_LINE, CMD_TOOL_ARROW, CMD_FILL] {
        assert!(menu_enabled(&app, cmd), "{cmd}");
    }
    app.event(Event::Command(CMD_TOOL_RECT));
    assert_eq!(app.edit.tool, Tool::Shape(ShapeKind::Rectangle));
    // A click is not a shape.
    click(&mut app, (500, 500));
    assert_eq!(app.edit.annots.get(&0).unwrap().len(), before);
    assert!(!app.edit.dirty);
    // Dragged with Shift held: a square, as long as the longer side.
    let from = (400, 400);
    down(&mut app, from, 1);
    app.event(Event::MouseMove { x: 460, y: 440, shift: true });
    let mut painted = sidebar::tests::Rec::default();
    app.paint(&mut painted);
    assert!(!painted.fills.is_empty());
    up(&mut app, (460, 440));
    pump(&mut app, "the shape", |a| a.edit.dirty && a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Square" && i.index == 7)));
    let made = info(&app, 7);
    let (w, h) = (made.rect[2] - made.rect[0], made.rect[3] - made.rect[1]);
    assert!((w - h).abs() < 0.8 && w > 20.0, "a square: {w} x {h}");
    assert_eq!(made.author, "Tester");
    // An arrow at an angle off a multiple of 45 degrees comes out at 45 with Shift.
    app.event(Event::Command(CMD_TOOL_ARROW));
    drag(&mut app, (300, 600), (400, 680), true);
    pump(&mut app, "the arrow", |a| a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Line" && i.index == 8)));
    let arrow = info(&app, 8).line.expect("a line");
    assert!(((arrow[2] - arrow[0]).abs() - (arrow[3] - arrow[1]).abs()).abs() < 1.5, "{arrow:?}");
    // Undone, both are gone again.
    app.event(Event::Command(CMD_UNDO));
    app.event(Event::Command(CMD_UNDO));
    pump(&mut app, "the undo", |a| a.edit.annots.get(&0).is_some_and(|l| l.len() == before) && !a.edit.can_undo);
    // The fill switch is a menu item that is checked once it is on.
    app.event(Event::Command(CMD_FILL));
    assert!(app.edit.ae.fill);
}

#[test]
fn an_annotation_is_picked_moved_and_resized_by_the_pointer_and_undone_each_in_one_step() {
    let mut app = open("pointer");
    let start = rect_of(&app, SQUARE);
    // A click picks it, with its handles; the status line tells what it is.
    let at = middle(&app, SQUARE);
    click(&mut app, at);
    assert_eq!(app.edit.picked_info().map(|a| a.index), Some(SQUARE as u32));
    assert!(app.note.as_deref().is_some_and(|n| n.starts_with("Rectangle - Ann")), "{:?}", app.note);
    // Dragged 40 points right and 30 down (at the zoom of the window: pixels / scale).
    let scale = app.doc.as_ref().unwrap().layout.scale();
    let px = |pt: f32| (f64::from(pt) * scale).round() as i32;
    drag(&mut app, at, (at.0 + px(40.0), at.1 + px(30.0)), false);
    settled(&mut app);
    near(rect_of(&app, SQUARE), [start[0] + 40.0, start[1] + 30.0, start[2] + 40.0, start[3] + 30.0], 1.5);
    assert!(app.edit.dirty && app.edit.picked_info().is_some(), "still picked");
    assert!(app.note.as_deref().is_some_and(|n| n.contains("changed")), "{:?}", app.note);
    // Its lower right corner dragged 20 points further out: a bigger square, the same upper left corner.
    let moved = rect_of(&app, SQUARE);
    let corner = {
        let r = app.window_rect(0, moved).unwrap();
        (r.x + r.w, r.y + r.h)
    };
    drag(&mut app, corner, (corner.0 + px(20.0), corner.1 + px(20.0)), false);
    settled(&mut app);
    let grown = rect_of(&app, SQUARE);
    near([grown[0], grown[1], grown[2], grown[3]], [moved[0], moved[1], moved[2] + 20.0, moved[3] + 20.0], 2.0);
    // Each is one step: two undos bring the first rectangle back.
    app.event(Event::Command(CMD_UNDO));
    app.event(Event::Command(CMD_UNDO));
    pump(&mut app, "the undos", |a| !a.edit.can_undo && a.edit.annots.get(&0).is_some_and(|l| l.len() == 7));
    near(rect_of(&app, SQUARE), start, 0.01);
    assert!(!app.edit.dirty);
}

#[test]
fn the_arrow_keys_nudge_the_annotation_that_is_picked_a_point_or_ten_and_a_burst_is_one_step() {
    let mut app = open("nudge");
    let start = rect_of(&app, LINE);
    let at = middle(&app, LINE);
    // (A line is picked by its line, not by its box: the middle of the box is on it.)
    click(&mut app, at);
    assert_eq!(app.edit.picked_info().map(|a| a.index), Some(LINE as u32));
    let key = |app: &mut App, vk: u32, shift: bool| app.event(Event::Key { vk, ctrl: false, shift });
    let page_scroll = app.scroll_y;
    for _ in 0..3 {
        key(&mut app, vk::RIGHT, false);
    }
    key(&mut app, vk::DOWN, true);
    assert_eq!(app.scroll_y, page_scroll, "the keys did not scroll the page");
    assert!(!app.edit.dirty, "nothing is sent until the keys stop");
    // The timer says they have: one move of 3 points right and 10 down.
    app.event(Event::Timer(TIMER_NUDGE));
    settled(&mut app);
    near(rect_of(&app, LINE), [start[0] + 3.0, start[1] + 10.0, start[2] + 3.0, start[3] + 10.0], 0.01);
    app.event(Event::Command(CMD_UNDO));
    pump(&mut app, "the undo", |a| !a.edit.can_undo && a.edit.asked.is_empty());
    settled(&mut app);
    near(rect_of(&app, LINE), start, 0.01);
    // With nothing picked the arrow keys scroll as they did.
    app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
    key(&mut app, vk::DOWN, false);
    assert!(app.scroll_y > page_scroll);
}

#[test]
fn what_is_locked_is_picked_but_not_moved_resized_recoloured_or_taken_out() {
    let mut app = open("locked");
    let start = rect_of(&app, LOCKED);
    let at = middle(&app, LOCKED);
    click(&mut app, at);
    assert_eq!(app.edit.picked_info().map(|a| a.index), Some(LOCKED as u32));
    drag(&mut app, at, (at.0 + 60, at.1 + 60), false);
    app.event(Event::Key { vk: vk::DELETE, ctrl: false, shift: false });
    assert!(app.note.as_deref().is_some_and(|n| n.contains("locked")), "{:?}", app.note);
    // The menu has nothing to change it with and nothing to take it out.
    assert!(!menu_enabled(&app, CMD_COLOR_BASE) && !menu_enabled(&app, CMD_WIDTH_BASE) && !menu_enabled(&app, CMD_REMOVE));
    std::thread::sleep(Duration::from_millis(50));
    app.event(Event::Wake);
    assert!(app.edit.asked.is_empty() && !app.edit.dirty);
    near(rect_of(&app, LOCKED), start, 0.01);
}

#[test]
fn the_colour_and_the_width_menus_change_the_annotation_picked_and_are_greyed_for_what_the_reader_cannot_draw() {
    let mut app = open("restyle");
    let at = middle(&app, SQUARE);
    click(&mut app, at);
    assert!(menu_enabled(&app, CMD_COLOR_BASE + 2) && menu_enabled(&app, CMD_WIDTH_BASE + 3) && menu_enabled(&app, CMD_FILL));
    // Blue, thick, filled: the same rectangle, drawn again.
    let before = rect_of(&app, SQUARE);
    app.event(Event::Command(CMD_COLOR_BASE + 2));
    settled(&mut app);
    app.event(Event::Command(CMD_WIDTH_BASE + 3));
    settled(&mut app);
    app.event(Event::Command(CMD_FILL));
    settled(&mut app);
    // (The shape stays on the middle of its line, so the rectangle only grows with the pen.)
    let after = rect_of(&app, SQUARE);
    assert!(after[0] <= before[0] + 0.01 && after[1] <= before[1] + 0.01 && after[2] >= before[2] - 0.01 && after[3] >= before[3] - 0.01, "{after:?} / {before:?}");
    near(after, before, 12.0);
    assert!(app.edit.picked_info().is_some(), "still picked");
    assert!(!app.edit.ae.fill, "the choice was for the annotation, not for the next ones");
    // A stamp: nothing to draw again, the items are greyed.
    let at = middle(&app, STAMP);
    click(&mut app, at);
    assert_eq!(app.edit.picked_info().map(|a| a.index), Some(STAMP as u32));
    assert!(!menu_enabled(&app, CMD_COLOR_BASE) && !menu_enabled(&app, CMD_WIDTH_BASE) && !menu_enabled(&app, CMD_FILL));
    // A note has a colour but no width; a highlight picked by the click when the button comes up.
    let at = middle(&app, NOTE);
    click(&mut app, at);
    assert!(menu_enabled(&app, CMD_COLOR_BASE) && !menu_enabled(&app, CMD_WIDTH_BASE));
    let at = middle(&app, HIGHLIGHT);
    click(&mut app, at);
    assert_eq!(app.edit.picked_info().map(|a| a.index), Some(HIGHLIGHT as u32));
    app.event(Event::Command(CMD_COLOR_BASE + 3));
    settled(&mut app);
    assert!(app.edit.picked_info().is_some());
    // The edits are undone one at a time, the last first.
    app.event(Event::Command(CMD_UNDO));
    pump(&mut app, "the undo", |a| a.edit.can_redo);
}

#[test]
fn a_double_click_types_over_the_text_of_a_note_in_the_box_for_notes_and_of_a_text_box_in_place() {
    let mut app = open("typing");
    let at = middle(&app, NOTE);
    down(&mut app, at, 1);
    up(&mut app, at);
    let asked = down(&mut app, at, 2);
    up(&mut app, at);
    assert!(asked.is_empty() || !asked.iter().any(|a| matches!(a, Action::AskText { .. })), "the text is asked of the engine first");
    let mut ask = Vec::new();
    for _ in 0..1000 {
        ask.extend(app.event(Event::Wake));
        if ask.iter().any(|a| matches!(a, Action::AskText { .. })) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ask.iter().any(|a| matches!(a, Action::AskText { tag: TAG_NOTE_EDIT, initial, .. } if initial == "hello note")), "{ask:?}");
    app.event(Event::Text { tag: TAG_NOTE_EDIT, text: Some("hello \u{4E16}\u{754C}".to_string()) });
    settled(&mut app);
    assert_eq!(info(&app, NOTE).contents, "hello \u{4E16}\u{754C}");
    // The text box: its own box opens over it, with its text.
    let at = middle(&app, TEXTBOX);
    click(&mut app, at);
    down(&mut app, at, 2);
    up(&mut app, at);
    let mut open_box = Vec::new();
    for _ in 0..1000 {
        open_box.extend(app.event(Event::Wake));
        if open_box.iter().any(|a| matches!(a, Action::FieldEdit(Some(_)))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(open_box.iter().any(|a| matches!(a, Action::FieldEdit(Some(b)) if b.text == "box text" && b.multiline)), "{open_box:?}");
    app.event(Event::FieldDone { text: "new text\nsecond line".to_string(), exit: FieldExit::Commit });
    settled(&mut app);
    assert_eq!(info(&app, TEXTBOX).contents, "new text second line");
    // Esc leaves it alone; the locked square's text is not asked for.
    let at = middle(&app, LOCKED);
    click(&mut app, at);
    down(&mut app, at, 2);
    up(&mut app, at);
    assert!(app.edit.ae.text_ask.is_none());
}

#[test]
fn the_third_tab_lists_the_annotations_by_page_and_a_click_goes_to_the_annotation_and_delete_takes_it_out() {
    let mut app = open("list");
        // The sidebar is opened on the annotations (F4 on a file without bookmarks shows the thumbnails; the tab is clicked).
    app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false });
    assert!(app.view_x > 0);
    let third = (app.list_w() - 10) as i32;
    click(&mut app, (third, 10));
    assert_eq!(app.active_tab(), Tab::Annots);
    pump(&mut app, "the list", |a| a.al.loaded_rows() > 0);
    // A heading for each page and a row for each annotation: 2 + 8.
    assert_eq!(app.al.loaded_rows(), 10);
    let mut rec = sidebar::tests::Rec::default();
    app.paint(&mut rec);
    // A row is a picture of the kind, the title (the words, or the kind when there are none) and who made it and when.
    for want in ["Page 1", "Page 2", "Bob \u{b7} 2026-10-02", "Ann \u{b7} 2026-10-01", "hello note", "a square", "Line"] {
        assert!(rec.texts.iter().any(|(_, _, s)| s == want), "{want} is not drawn: {:?}", rec.texts);
    }
    assert!(rec.texts.iter().any(|(_, _, s)| s == "\u{e70b}"), "the picture of a note");
    // Click the row of the second page's note: the page is shown and the note is picked.
    let row_y = |app: &App, key: (u32, u32)| {
        let r = app.al.row_of(key).expect("a row");
        (app.px(sidebar::TAB_H) + app.row_top(r) - app.al.scroll + 4) as i32
    };
    let y = row_y(&app, (1, 0));
    click(&mut app, (40, y));
    pump(&mut app, "the second page's note to be picked", |a| a.edit.picked_info().is_some_and(|i| i.contents == "second page"));
    assert_eq!(app.edit.picked.map(|p| p.0), Some(1));
    assert!(app.scroll_y > 0);
    // Delete in the list takes the annotation out (the list is made again without it); a locked one is refused.
    let before = app.al.loaded_rows();
    app.event(Event::Key { vk: vk::DELETE, ctrl: false, shift: false });
    pump(&mut app, "the removal", |a| a.al.loaded_rows() < before && a.edit.asked.is_empty());
    assert_eq!(app.al.loaded_rows(), before - 2, "the note and the heading of its page");
    let y = row_y(&app, (0, LOCKED as u32));
    click(&mut app, (40, y));
    app.event(Event::Key { vk: vk::DELETE, ctrl: false, shift: false });
    assert!(app.note.as_deref().is_some_and(|n| n.contains("locked")), "{:?}", app.note);
    // A file without annotations says so.
    let mut empty = {
        let file = std::env::temp_dir().join(format!("qingpdf-view-annot-empty-{}.pdf", std::process::id()));
        std::fs::write(&file, pdf(&[(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()), (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()), (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>".to_string())])).unwrap();
        let mut a = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        a.persist = false;
        a.recent_path = None;
        a.apply_settings(&crate::settings::Settings { tab: Some(Tab::Annots), ..crate::settings::Settings::default() });
        a.event(Event::Size { w: 1000, h: 700 });
        a.open_path(&file, "");
        a
    };
    pump(&mut empty, "the empty list", |a| a.has_document() && a.al.is_loaded());
    let mut rec = sidebar::tests::Rec::default();
    empty.paint(&mut rec);
    assert!(rec.texts.iter().any(|(_, _, s)| s == "There are no annotations in this file."), "{:?}", rec.texts);
}

#[test]
fn the_row_the_pointer_is_over_in_the_list_of_annotations_has_the_pale_fill_and_a_heading_has_none() {
    let mut app = open("hover");
    app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false });
    let third = (app.list_w() - 10) as i32;
    click(&mut app, (third, 10));
    pump(&mut app, "the list", |a| a.al.loaded_rows() > 0);
    let row = app.al.row_of((0, NOTE as u32)).expect("a row");
    let y = (app.tabs_h() + app.row_top(row) - app.al.scroll + 4) as i32;
    assert!(app.event(Event::Hover { x: 60, y }).iter().any(|a| matches!(a, Action::Invalidate)));
    assert_eq!(app.side_hot, Some(row));
    let mut rec = sidebar::tests::Rec::default();
    app.paint(&mut rec);
    assert!(rec.fills.iter().any(|(_, c)| *c == app.pal.status_fg), "the row under the pointer is filled");
    // The same row again changes nothing; the heading of the page and the pointer leaving the window take the fill away.
    assert!(app.event(Event::Hover { x: 60, y: y + 1 }).is_empty());
    app.event(Event::Hover { x: 60, y: (app.tabs_h() + 2) as i32 });
    assert_eq!(app.side_hot, None);
    app.event(Event::Hover { x: 60, y });
    app.event(Event::HoverEnd);
    assert_eq!(app.side_hot, None);
}

#[test]
fn five_hundred_annotations_on_a_page_are_picked_and_drawn_within_a_frame() {
    // 500 squares in a grid.
    let mut annots = Vec::new();
    let mut objects = vec![(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()), (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string())];
    for i in 0..500u32 {
        let (x, y) = (20.0 + f64::from(i % 25) * 22.0, 20.0 + f64::from(i / 25) * 36.0);
        objects.push((10 + i, format!("<< /Type /Annot /Subtype /Square /Rect [{x} {y} {} {}] /C [1 0 0] /F 4 >>", x + 18.0, y + 30.0)));
        annots.push(format!("{} 0 R", 10 + i));
    }
    objects.push((3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [{}] >>", annots.join(" "))));
    let file = std::env::temp_dir().join(format!("qingpdf-view-annot-500-{}.pdf", std::process::id()));
    std::fs::write(&file, pdf(&objects)).unwrap();
    let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
    app.persist = false;
    app.recent_path = None;
    app.event(Event::Size { w: 1000, h: 900 });
    app.open_path(&file, "");
    pump(&mut app, "the file", |a| a.has_document());
    app.event(Event::Command(CMD_FIT_PAGE));
    pump(&mut app, "500 annotations", |a| a.edit.annots.get(&0).is_some_and(|l| l.len() == 500));
    // (One near the top of the page, which is in the window.)
    let at = middle_of(&app, 487);
    let started = Instant::now();
    for _ in 0..50 {
        click(&mut app, at);
        let mut rec = sidebar::tests::Rec::default();
        app.paint(&mut rec);
    }
    let each = started.elapsed() / 50;
    assert_eq!(app.edit.picked_info().map(|a| a.index), Some(487));
    println!("pick and redraw with 500 annotations: {each:?} each");
    assert!(each < Duration::from_millis(16), "{each:?} for one pick and one drawing");
    // The drag of one of them: each move is a frame.
    let started = Instant::now();
    down(&mut app, at, 1);
    for i in 1..=50 {
        app.event(Event::MouseMove { x: at.0 + i, y: at.1 + i, shift: false });
        let mut rec = sidebar::tests::Rec::default();
        app.paint(&mut rec);
    }
    let each = started.elapsed() / 50;
    up(&mut app, (at.0 + 50, at.1 + 50));
    println!("a move with 500 annotations: {each:?} each");
    assert!(each < Duration::from_millis(16), "{each:?} for one move and one drawing");
}

fn middle_of(app: &App, which: u32) -> (i32, i32) {
    let a = app.edit.annots.get(&0).and_then(|l| l.iter().find(|a| a.index == which)).expect("listed");
    let r = app.window_rect(0, a.rect).expect("on the window");
    (r.x + r.w / 2, r.y + r.h / 2)
}
