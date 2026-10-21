//! Cropping, the pages and pictures as files, and the properties box (2026-10-21), through the reader's events and the real engine.

use std::path::PathBuf;
use std::time::Duration;

use qingpdf_core::export::file_stem;

use super::sidebar::tests::{app, letter, open_pages, pump};
use super::*;
use crate::ui::{FormAnswer, FormKind, FormSpec, vk};

fn form_of(actions: &[Action]) -> FormSpec {
    actions.iter().find_map(|a| if let Action::AskForm(f) = a { Some(f.clone()) } else { None }).unwrap_or_else(|| panic!("no box: {actions:?}"))
}

fn answer(app: &mut App, tag: u32, values: &[&str]) -> Vec<Action> {
    app.event(Event::Form { tag, answer: Some(FormAnswer { values: values.iter().map(ToString::to_string).collect(), button: None }) })
}

/// Pump the engine until `until`, giving all the actions the reader answered with.
fn pump_actions(app: &mut App, what: &str, until: impl Fn(&App) -> bool) -> Vec<Action> {
    let mut all = Vec::new();
    for _ in 0..3000 {
        all.extend(app.event(Event::Wake));
        if until(app) {
            return all;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn size_of(app: &App, page: usize) -> (f64, f64) {
    app.doc.as_ref().and_then(|d| d.layout.page_size_pt(page)).expect("a page")
}

fn sizes(app: &App) -> Vec<(i64, i64)> {
    (0..app.page_count()).map(|p| {
        let (w, h) = size_of(app, p);
        (w.round() as i64, h.round() as i64)
    }).collect()
}

fn enabled(app: &App, cmd: u32) -> bool {
    fn find(entries: &[crate::ui::MenuEntry], cmd: u32) -> Option<bool> {
        entries.iter().find_map(|e| match e {
            crate::ui::MenuEntry::Item { id, enabled, .. } if *id == cmd => Some(*enabled),
            crate::ui::MenuEntry::Submenu { entries, .. } => find(entries, cmd),
            _ => None,
        })
    }
    app.menus().iter().find_map(|m| find(&m.entries, cmd)).expect("in a menu")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qingpdf-view-tools-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder");
    dir
}

#[test]
fn margins_crop_the_pages_the_box_names_and_undo_gives_them_back() {
    let mut app = open_pages("crop-margins", &letter(4), true);
    assert!(enabled(&app, CMD_CROP) && enabled(&app, CMD_UNCROP) && enabled(&app, CMD_TOOL_CROP));
    let form = form_of(&app.event(Event::Command(CMD_CROP)));
    assert_eq!(form.tag, TAG_CROP);
    assert_eq!(form.items.iter().filter(|i| i.kind == FormKind::Edit).count(), 5);
    assert_eq!(form.items.iter().filter(|i| i.kind == FormKind::Pick).map(|i| i.choices.len()).collect::<Vec<_>>(), [6]);
    assert!(form.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= form.size.0 && i.rect.1 + i.rect.3 <= form.size.1 || i.kind == FormKind::Pick));
    // Cancelled, or answered with something that is not the box's values: nothing happens.
    assert!(app.event(Event::Form { tag: TAG_CROP, answer: None }).is_empty());
    assert!(answer(&mut app, TAG_CROP, &["1"]).is_empty());
    // Not numbers, or more than the page: said, and nothing is asked.
    answer(&mut app, TAG_CROP, &["x", "0", "0", "0", "0", "0", ""]);
    assert_eq!(app.note.as_deref(), Some(t(Lang::En, Msg::NoteCropBad)));
    answer(&mut app, TAG_CROP, &["200", "0", "200", "0", "0", "0", ""]);
    assert!(!app.edit.dirty);
    // 10, 20, 30 and 40 millimetres off the top, right, bottom and left of this page.
    answer(&mut app, TAG_CROP, &["10", "20", "30", "40", "0", "0", ""]);
    pump(&mut app, "the crop", |a| a.edit.dirty && size_of(a, 0).0 < 500.0);
    let (w, h) = size_of(&app, 0);
    assert!((w - (612.0 - 60.0 * 72.0 / 25.4)).abs() < 0.1 && (h - (792.0 - 40.0 * 72.0 / 25.4)).abs() < 0.1, "{w} x {h}");
    assert_eq!(sizes(&app)[1..], [(612, 792); 3], "the other pages are as they were");
    assert!(app.note.as_deref().is_some_and(|n| n.contains("Cropped")));
    app.event(Event::Command(CMD_UNDO));
    pump(&mut app, "the undo", |a| size_of(a, 0).0 > 600.0 && !a.edit.dirty);
    // The odd pages (the first and the third), a range, and the pages chosen in the thumbnails.
    answer(&mut app, TAG_CROP, &["10", "10", "10", "10", "0", "4", ""]);
    pump(&mut app, "the odd pages", |a| size_of(a, 2).0 < 600.0);
    assert_eq!(sizes(&app).iter().map(|s| s.0 < 600).collect::<Vec<_>>(), [true, false, true, false]);
    answer(&mut app, TAG_CROP, &["0", "0", "0", "30", "0", "3", "2-3"]);
    pump(&mut app, "the range", |a| size_of(a, 1).0 < 600.0);
    assert_eq!(sizes(&app).iter().map(|s| s.0 < 520).collect::<Vec<_>>(), [false, false, true, false], "page 3 has had both");
    // Uncrop: the whole paper again for the pages named; the pages that are whole are left alone.
    let form = form_of(&app.event(Event::Command(CMD_UNCROP)));
    assert_eq!(form.tag, TAG_UNCROP);
    answer(&mut app, TAG_UNCROP, &["2", ""]);
    pump(&mut app, "the uncrop", |a| sizes(a).iter().all(|s| *s == (612, 792)));
    answer(&mut app, TAG_UNCROP, &["2", ""]);
    pump(&mut app, "the answer", |a| a.edit.asked.is_empty());
    assert!(app.note.as_deref().is_some_and(|n| n.contains("failed") || n.contains("none") || n.contains("not cropped")), "{:?}", app.note);
}

#[test]
fn a_box_dragged_on_the_page_fills_in_the_margins() {
    let mut app = open_pages("crop-drag", &letter(3), true);
    app.event(Event::Command(CMD_FIT_PAGE));
    app.event(Event::Command(CMD_TOOL_CROP));
    assert_eq!(app.edit.tool, Tool::Crop);
    assert!(app.note.as_deref().is_some_and(|n| n.contains("Drag a box")));
    // A click is not a box.
    let r = app.window_rect(0, [100.0, 100.0, 400.0, 500.0]).expect("on the window");
    app.event(Event::MouseDown { x: r.x, y: r.y, ctrl: false, shift: false, clicks: 1 });
    assert!(!app.event(Event::MouseUp { x: r.x, y: r.y }).iter().any(|a| matches!(a, Action::AskForm(_))));
    // A box from (100, 100) to (400, 500) points: 100 off the top and the left, 212 off the right, 292 off the bottom.
    app.event(Event::MouseDown { x: r.x, y: r.y, ctrl: false, shift: false, clicks: 1 });
    app.event(Event::MouseMove { x: r.x + r.w / 2, y: r.y + r.h / 2, shift: false });
    let mut seen = sidebar::tests::Rec::default();
    app.paint(&mut seen);
    app.event(Event::MouseMove { x: r.x + r.w, y: r.y + r.h, shift: false });
    let actions = app.event(Event::MouseUp { x: r.x + r.w, y: r.y + r.h });
    let form = form_of(&actions);
    assert_eq!(form.tag, TAG_CROP);
    let margins: Vec<f64> = form.items.iter().filter(|i| i.kind == FormKind::Edit).take(4).map(|i| i.text.parse::<f64>().expect("a number")).collect();
    let want = [100.0, 212.0, 292.0, 100.0].map(|pt| pt * 25.4 / 72.0);
    assert!(margins.iter().zip(want).all(|(got, want)| (got - want).abs() < 1.5), "{margins:?} against {want:?}");
    // The tool is over and the box asks about the page the box was dragged on.
    assert_eq!(app.edit.tool, Tool::Select);
    assert_eq!(form.items.iter().find(|i| i.kind == FormKind::Pick).map(|i| i.selected), Some(0));
}

#[test]
fn the_white_margins_are_found_by_the_engine_and_esc_stops_the_looking() {
    let mut app = open_pages("crop-auto", &letter(3), true);
    // The pages show a rectangle from (10, 10) to (602, 406) in user space: the lower half, nearly the page's width.
    answer(&mut app, TAG_CROP, &["0", "0", "0", "0", "1", "2", ""]);
    assert!(app.cr.busy() && app.note.as_deref().is_some_and(|n| n.contains("white margins")));
    pump(&mut app, "the crop", |a| a.edit.dirty && size_of(a, 2).1 < 600.0);
    for page in 0..3 {
        let (w, h) = size_of(&app, page);
        assert!((w - 609.0).abs() < 6.0 && (h - 413.0).abs() < 6.0, "page {page}: {w} x {h}");
    }
    // Looking again at a big page and then Esc: the request is taken back and nothing is cropped.
    let mut app = open_pages("crop-auto-esc", &letter(30), true);
    answer(&mut app, TAG_CROP, &["0", "0", "0", "0", "1", "2", ""]);
    assert!(app.cr.busy());
    app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
    assert!(!app.cr.busy() && app.note.as_deref() == Some("Stopped."));
    std::thread::sleep(Duration::from_millis(300));
    app.event(Event::Wake);
    assert!(!app.edit.dirty);
}

#[test]
fn cropping_needs_the_right_to_change_pages_or_content() {
    let mut app = open_pages("crop-rights", &letter(2), true);
    app.rights.assemble = false;
    assert!(enabled(&app, CMD_CROP), "bit 4 is enough");
    app.rights.modify = false;
    app.menus_dirty = true;
    assert!(!enabled(&app, CMD_CROP) && !enabled(&app, CMD_TOOL_CROP) && !enabled(&app, CMD_UNCROP));
    assert!(app.event(Event::Command(CMD_CROP)).iter().all(|a| !matches!(a, Action::AskForm(_))));
    assert!(app.note.is_some());
    // The tool is not taken either.
    app.event(Event::Command(CMD_TOOL_CROP));
    assert_eq!(app.edit.tool, Tool::Select);
}

#[test]
fn the_pages_are_written_as_pictures_in_a_folder_and_esc_stops_it() {
    let mut app = open_pages("export", &letter(4), true);
    let stem = file_stem(&app.path.as_ref().unwrap().file_name().unwrap().to_string_lossy());
    let form = form_of(&app.event(Event::Command(CMD_EXPORT_IMAGES)));
    assert_eq!(form.tag, TAG_EXPORT_IMAGES);
    let picks: Vec<usize> = form.items.iter().filter(|i| i.kind == FormKind::Pick).map(|i| i.choices.len()).collect();
    assert_eq!(picks, [3, 2, 4]);
    assert!(form.items.iter().find(|i| i.kind == FormKind::Edit).is_some_and(|i| i.text == "90"));
    // OK: where to write is asked; cancelling that does nothing.
    let asked = answer(&mut app, TAG_EXPORT_IMAGES, &["2", "1", "75", "0"]);
    assert!(matches!(&asked[..], [Action::PickFolder { .. }]));
    assert!(app.event(Event::FolderPicked(None)).is_empty() && !app.px.busy());
    answer(&mut app, TAG_EXPORT_IMAGES, &["2", "1", "75", "0"]);
    let dir = scratch("export");
    app.event(Event::FolderPicked(Some(dir.clone())));
    assert!(app.px.busy() && app.note.as_deref().is_some_and(|n| n.contains("Writing pictures")));
    // Another export, a crop or taking pages out meanwhile: busy.
    assert!(app.event(Event::Command(CMD_EXPORT_IMAGES)).iter().all(|a| !matches!(a, Action::AskForm(_))));
    let done = pump_actions(&mut app, "the pictures", |a| !a.px.busy());
    let said = done.iter().find_map(|a| if let Action::Message { text, .. } = a { Some(text.clone()) } else { None }).expect("told");
    assert!(said.contains("4 picture") && said.contains(&dir.to_string_lossy().into_owned()), "{said}");
    let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, (1..=4).map(|n| format!("{stem}-{n}.jpg")).collect::<Vec<_>>());
    // 72 dpi: 612 by 792 pixels.
    let jpeg = std::fs::read(dir.join(format!("{stem}-1.jpg"))).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    // This page only, as PNG at 72 dpi; then the chosen thumbnails.
    answer(&mut app, TAG_EXPORT_IMAGES, &["0", "0", "90", "0"]);
    app.event(Event::FolderPicked(Some(dir.clone())));
    pump_actions(&mut app, "one picture", |a| !a.px.busy());
    assert!(dir.join(format!("{stem}-1.png")).is_file() && !dir.join(format!("{stem}-2.png")).exists());
    // Esc while it goes.
    let mut big = open_pages("export-esc", &letter(40), true);
    answer(&mut big, TAG_EXPORT_IMAGES, &["2", "0", "90", "2"]);
    let other = scratch("export-esc");
    big.event(Event::FolderPicked(Some(other.clone())));
    assert!(big.px.busy());
    big.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
    assert!(!big.px.busy() && big.note.as_deref() == Some("Stopped."));
    // A file that does not allow copying is not exported.
    let mut locked = open_pages("export-locked", &letter(2), true);
    locked.rights.copy = false;
    assert!(locked.event(Event::Command(CMD_EXPORT_IMAGES)).iter().all(|a| !matches!(a, Action::AskForm(_))));
    assert!(locked.event(Event::Command(CMD_EXTRACT_IMAGES)).iter().all(|a| !matches!(a, Action::AskForm(_))));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn pages_that_would_replace_files_are_asked_about_once_for_all_of_them() {
    let mut app = open_pages("export-replace", &letter(3), true);
    let stem = file_stem(&app.path.as_ref().unwrap().file_name().unwrap().to_string_lossy());
    let dir = scratch("export-replace");
    // Pages 1 and 3 would take the place of files that are there.
    let (one, three) = (dir.join(format!("{stem}-1.png")), dir.join(format!("{stem}-3.png")));
    std::fs::write(&one, b"mine").unwrap();
    std::fs::write(&three, b"mine too").unwrap();
    // (A file of another kind of picture, which a PNG does not replace, is not asked about.)
    std::fs::write(dir.join(format!("{stem}-2.jpg")), b"not in the way").unwrap();
    answer(&mut app, TAG_EXPORT_IMAGES, &["2", "0", "90", "0"]);
    let asked = app.event(Event::FolderPicked(Some(dir.clone())));
    let Some(Action::Confirm { tag, text, .. }) = asked.iter().find(|a| matches!(a, Action::Confirm { .. })) else { panic!("not asked: {asked:?}") };
    assert_eq!(*tag, TAG_EXPORT_REPLACE);
    assert!(text.contains("2 file(s)") && text.contains(&format!("{stem}-1.png")), "{text}");
    assert_eq!(asked.iter().filter(|a| matches!(a, Action::Confirm { .. })).count(), 1);
    // No: nothing is written, nothing is replaced, and the reader is free again.
    assert!(app.px.busy());
    assert!(app.event(Event::Confirmed { tag: TAG_EXPORT_REPLACE, yes: false }).is_empty());
    assert!(!app.px.busy());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(std::fs::read(&one).unwrap(), b"mine");
    assert!(!dir.join(format!("{stem}-2.png")).exists());
    // Yes: all three pages are written, the two files are replaced.
    answer(&mut app, TAG_EXPORT_IMAGES, &["2", "0", "90", "0"]);
    app.event(Event::FolderPicked(Some(dir.clone())));
    app.event(Event::Confirmed { tag: TAG_EXPORT_REPLACE, yes: true });
    assert!(app.px.busy());
    pump_actions(&mut app, "the pictures", |a| !a.px.busy());
    for n in 1..=3 {
        assert_eq!(&std::fs::read(dir.join(format!("{stem}-{n}.png"))).unwrap()[..4], b"\x89PNG", "page {n}");
    }
    // A folder with nothing in the way is not asked about.
    let fresh = scratch("export-replace-fresh");
    answer(&mut app, TAG_EXPORT_IMAGES, &["0", "0", "90", "0"]);
    let started = app.event(Event::FolderPicked(Some(fresh.clone())));
    assert!(started.iter().all(|a| !matches!(a, Action::Confirm { .. })) && app.px.busy());
    pump_actions(&mut app, "the picture", |a| !a.px.busy());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&fresh);
}

#[test]
fn undo_and_redo_wait_while_pages_are_written_or_looked_at_for_their_margins() {
    let mut app = open_pages("busy-undo", &letter(40), true);
    answer(&mut app, TAG_CROP, &["10", "10", "10", "10", "0", "0", ""]);
    pump(&mut app, "the crop", |a| a.edit.dirty && a.edit.can_undo && a.edit.asked.is_empty());
    // Pages are being written: the undo says so and does not ask the engine (the pages would change under the work).
    answer(&mut app, TAG_EXPORT_IMAGES, &["2", "0", "90", "2"]);
    let dir = scratch("busy-undo");
    app.event(Event::FolderPicked(Some(dir.clone())));
    assert!(app.px.busy());
    app.event(Event::Command(CMD_UNDO));
    assert!(app.edit.asked.is_empty(), "the engine was not asked");
    assert_eq!(app.note.as_deref(), Some(t(Lang::En, Msg::NoteBusy)));
    app.event(Event::Command(CMD_REDO));
    assert!(app.edit.asked.is_empty());
    app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
    assert!(!app.px.busy());
    // While the margins are looked for, the same.
    answer(&mut app, TAG_CROP, &["0", "0", "0", "0", "1", "2", ""]);
    assert!(app.cr.busy());
    app.note = None;
    app.event(Event::Command(CMD_UNDO));
    assert!(app.edit.asked.is_empty() && app.note.as_deref() == Some(t(Lang::En, Msg::NoteBusy)), "{:?}", app.note);
    app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
    assert!(!app.cr.busy());
    // Free again: the undo goes to the engine.
    std::thread::sleep(Duration::from_millis(300));
    app.event(Event::Wake);
    app.event(Event::Command(CMD_UNDO));
    assert!(!app.edit.asked.is_empty());
    pump(&mut app, "the undo", |a| a.edit.asked.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_page_that_is_all_picture_has_no_white_margin_to_cut_and_grey_paper_is_cropped_to_its_content() {
    // Four colours in four quarters, edge to edge: no paper, nothing to cut, said so, nothing changed.
    let quarters = "1 0 0 rg 0 396 306 396 re f 0 1 0 rg 306 396 306 396 re f 0 0 1 rg 0 0 306 396 re f 1 1 0 rg 306 0 306 396 re f";
    let mut app = open_bytes("crop-whole", one_page_pdf(quarters));
    answer(&mut app, TAG_CROP, &["0", "0", "0", "0", "1", "0", ""]);
    let done = pump_actions(&mut app, "the look", |a| !a.cr.busy());
    let Some(Action::Message { text, .. }) = done.iter().find(|a| matches!(a, Action::Message { .. })) else { panic!("{done:?}") };
    assert!(text.contains("fills them to the edges") && text.contains("There is no white margin to cut."), "{text}");
    assert!(!app.edit.dirty && app.edit.asked.is_empty());
    // A scan on grey paper (232, darker than the limit white paper used to have): the paper is the margin, the block is the content.
    let mut app = open_bytes("crop-grey", one_page_pdf("0.91 g 0 0 612 792 re f 0 g 100 300 400 200 re f"));
    answer(&mut app, TAG_CROP, &["0", "0", "0", "0", "1", "0", ""]);
    pump(&mut app, "the crop", |a| a.edit.dirty && size_of(a, 0).0 < 500.0);
    let (w, h) = size_of(&app, 0);
    assert!((w - (400.0 + 17.0)).abs() < 12.0 && (h - (200.0 + 17.0)).abs() < 12.0, "{w} x {h}");
}

#[test]
fn the_pictures_of_pages_are_extracted_and_a_page_with_none_is_told() {
    let mut app = open_pages("extract", &letter(2), true);
    let form = form_of(&app.event(Event::Command(CMD_EXTRACT_IMAGES)));
    assert_eq!((form.tag, form.items.iter().filter(|i| i.kind == FormKind::Pick).count()), (TAG_EXTRACT_IMAGES, 1));
    let dir = scratch("extract");
    assert!(matches!(&answer(&mut app, TAG_EXTRACT_IMAGES, &["2"])[..], [Action::PickFolder { .. }]));
    app.event(Event::FolderPicked(Some(dir.clone())));
    let done = pump_actions(&mut app, "the answer", |a| !a.px.busy());
    let said = done.iter().find_map(|a| if let Action::Message { text, .. } = a { Some(text.clone()) } else { None }).expect("told");
    assert!(said.contains("No pictures"), "{said}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file made of these objects (number and body) with this in its trailer, written the plain way.
fn built(objects: &[(u32, String)], trailer: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R {trailer} >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

/// A file of one page of 612 by 792 points that draws this.
fn one_page_pdf(content: &str) -> Vec<u8> {
    built(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>".to_string()),
            (4, format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len())),
        ],
        "",
    )
}

fn open_bytes(name: &str, bytes: Vec<u8>) -> App {
    let dir = scratch(name);
    let file = dir.join("file.pdf");
    std::fs::write(&file, bytes).unwrap();
    let mut app = app();
    app.event(Event::Size { w: 1000, h: 900 });
    app.open_path(&file, "");
    pump(&mut app, "the file to open", |a| a.has_document());
    app
}

/// A one-page file with a document information dictionary, an XMP stream and a font.
fn titled_pdf() -> Vec<u8> {
    titled_pdf_with("/Title (Old title) /Author (Writer) /Creator (Word) /Producer (Lib) /CreationDate (D:20260101080000+08'00')")
}

fn titled_pdf_with(info: &str) -> Vec<u8> {
    let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Old title</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>";
    let objects = [
        (1, "<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 6 0 R >> >> >>".to_string()),
        (4, format!("<< {info} >>")),
        (5, format!("<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{xmp}\nendstream", xmp.len())),
        (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()),
    ];
    built(&objects, "/Info 4 0 R")
}

fn open_titled(name: &str) -> App {
    let dir = scratch(name);
    let file = dir.join("titled.pdf");
    std::fs::write(&file, titled_pdf()).unwrap();
    let mut app = app();
    app.event(Event::Size { w: 1000, h: 900 });
    app.open_path(&file, "");
    pump(&mut app, "the file to open", |a| a.has_document());
    app
}

#[test]
fn the_properties_box_tells_about_the_file_and_changes_the_four_lines() {
    let mut app = open_titled("props");
    assert!(enabled(&app, CMD_PROPERTIES));
    // Ctrl+D asks; the box comes when the engine has gathered the properties.
    app.event(Event::Key { vk: vk::D, ctrl: true, shift: false });
    let actions = pump_actions(&mut app, "the box", |a| a.pp.shown.is_some());
    let form = form_of(&actions);
    assert_eq!(form.tag, TAG_PROPS);
    let text: String = form.items.iter().filter(|i| i.kind == FormKind::Label).map(|i| i.text.clone()).collect::<Vec<_>>().join("\n");
    for want in ["titled.pdf", "PDF 1.7, 1 page(s). Page 1: 209.9 x 297 mm (8.26 x 11.69 in)", "Made with: Word", "Producer: Lib", "Created: 2026-01-01 00:00:00 UTC", "Modified: -", "Signed: no   Tagged: no   Linearized: no", "Attachments: 0", "XMP: yes"] {
        assert!(text.contains(want), "{want:?} is not in\n{text}");
    }
    assert!(text.contains("No password, nothing restricted"));
    let edits: Vec<&str> = form.items.iter().filter(|i| i.kind == FormKind::Edit).map(|i| i.text.as_str()).collect();
    assert_eq!(edits, ["Old title", "Writer", "", ""]);
    let fonts = form.items.iter().find(|i| i.kind == FormKind::Pick).unwrap();
    assert_eq!(fonts.choices, ["Helvetica  [Type1, not embedded]"]);
    assert!(form.items.iter().any(|i| i.text == "Fonts used (1):"));
    // Nothing changed, or cancelled: nothing is asked of the engine.
    assert!(answer(&mut app, TAG_PROPS, &["Old title", "Writer", "", "", "0"]).is_empty());
    assert!(app.edit.asked.is_empty() && !app.edit.dirty);
    // The title and the keywords changed: one change, asked, done, the file is changed and the box shows the new values the next time.
    app.event(Event::Command(CMD_PROPERTIES));
    pump_actions(&mut app, "the box again", |a| a.pp.shown.is_some());
    answer(&mut app, TAG_PROPS, &["\u{65B0}\u{6807}\u{9898}", "Writer", "", "a, b", "0"]);
    pump(&mut app, "the change", |a| a.edit.dirty && a.edit.asked.is_empty());
    assert!(app.note.as_deref().is_some_and(|n| n.contains("Properties changed")));
    app.event(Event::Command(CMD_PROPERTIES));
    let form = form_of(&pump_actions(&mut app, "the box with the new values", |a| a.pp.shown.is_some()));
    let edits: Vec<&str> = form.items.iter().filter(|i| i.kind == FormKind::Edit).map(|i| i.text.as_str()).collect();
    assert_eq!(edits, ["\u{65B0}\u{6807}\u{9898}", "Writer", "", "a, b"]);
    app.pp.shown = None;
    // Undo: the old title.
    app.event(Event::Command(CMD_UNDO));
    pump(&mut app, "the undo", |a| !a.edit.dirty);
    app.event(Event::Command(CMD_PROPERTIES));
    let form = form_of(&pump_actions(&mut app, "the box after the undo", |a| a.pp.shown.is_some()));
    assert_eq!(form.items.iter().find(|i| i.kind == FormKind::Edit).map(|i| i.text.as_str()), Some("Old title"));
}

#[test]
fn what_a_file_says_about_itself_cannot_make_the_properties_box_taller_or_push_its_buttons_off() {
    let height_of = |app: &mut App| {
        app.event(Event::Command(CMD_PROPERTIES));
        let form = form_of(&pump_actions(app, "the box", |a| a.pp.shown.is_some()));
        app.pp.shown = None;
        form
    };
    let mut normal = open_titled("props-normal");
    let usual = height_of(&mut normal);
    // A creator and a producer of thousands of characters with line breaks and a right-to-left override in them; a title with a line break.
    let hex = |s: &str| format!("<FEFF{}>", s.encode_utf16().map(|u| format!("{u:04X}")).collect::<String>());
    let evil = "Evil\nline\r\n\u{202E}gnp.exe ".repeat(400);
    let dir = scratch("props-evil");
    let file = dir.join("evil.pdf");
    std::fs::write(&file, titled_pdf_with(&format!("/Title {} /Author (A) /Creator {} /Producer {}", hex("Two\nlines"), hex(&evil), hex(&evil)))).unwrap();
    let mut app = app();
    app.event(Event::Size { w: 1000, h: 900 });
    app.open_path(&file, "");
    pump(&mut app, "the file to open", |a| a.has_document());
    let form = height_of(&mut app);
    assert_eq!(form.size, usual.size, "the box is as tall as it is for any file");
    let labels: Vec<&str> = form.items.iter().filter(|i| i.kind == FormKind::Label).map(|i| i.text.as_str()).collect();
    for text in &labels {
        assert!(text.chars().all(|c| c == '\n' || !c.is_control()) && !text.contains('\u{202E}'), "{text:?}");
        assert!(text.lines().all(|l| l.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum::<usize>() <= 72), "a line that is longer than the box: {text:?}");
    }
    let made = labels.iter().flat_map(|t| t.lines()).find(|l| l.starts_with("Made with: ")).expect("a line for the creator");
    assert!(made.starts_with("Made with: Evil line") && made.ends_with("..."), "{made}");
    let edits: Vec<&str> = form.items.iter().filter(|i| i.kind == FormKind::Edit).map(|i| i.text.as_str()).collect();
    assert_eq!(edits[0], "Two lines", "a line break in the title is a space");
    // Every control is inside the box, and the last of them (the fonts) leaves room for the buttons.
    assert!(form.items.iter().all(|i| i.rect.1 + i.rect.3.min(24) <= form.size.1 - 40), "{:?}", form.items.iter().map(|i| (i.rect, i.kind)).collect::<Vec<_>>());
}

#[test]
fn a_file_that_does_not_allow_changing_it_shows_the_properties_but_not_to_edit() {
    let mut app = open_titled("props-locked");
    app.rights.modify = false;
    app.event(Event::Command(CMD_PROPERTIES));
    let form = form_of(&pump_actions(&mut app, "the box", |a| a.pp.shown.is_some()));
    assert!(form.items.iter().all(|i| i.kind != FormKind::Edit));
    let text: String = form.items.iter().filter(|i| i.kind == FormKind::Label).map(|i| i.text.clone()).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Old title") && text.contains("does not allow changing these"), "{text}");
    // Whatever it is answered with, nothing is asked.
    assert!(answer(&mut app, TAG_PROPS, &["x"]).is_empty());
    assert!(!app.edit.dirty);
    // Chinese: the labels are in Chinese.
    let mut zh = open_titled("props-zh");
    zh.set_language(LangChoice::Zh);
    zh.event(Event::Command(CMD_PROPERTIES));
    let form = form_of(&pump_actions(&mut zh, "the box in Chinese", |a| a.pp.shown.is_some()));
    assert_eq!(form.title, "\u{6587}\u{6863}\u{5C5E}\u{6027}");
}
