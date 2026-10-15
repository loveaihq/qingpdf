//! Insert > Page numbers, headers and footers: one box with the six places (each a template, with a list of the usual ones to pick from),
//! the size, colour and margin, the pages, the number the first one has, and the two switches. What is left in it is kept while the
//! program runs, so the next time it opens as it was. The box can also take away what an earlier use of it wrote (the engine finds
//! those by a private key of the page, `docs/decisions.md`).

use qingpdf_core::view::{MAX_MARGIN_MM, MAX_START, Pagination};

use super::editing::Asked;
use super::*;
use crate::ui::{FormAnswer, FormItem, FormKind, FormKind::*, FormPreview, FormSpec};

/// The templates the lists of the six places offer (`{page}` is the number of the page and `{pages}` the number of the last one).
const PRESETS: [&str; 6] = ["{page}", "第 {page} 页", "第 {page} 页 共 {pages} 页", "- {page} -", "{page} / {pages}", "Page {page} of {pages}"];
/// The colours of the text, as the box names them.
const COLORS: [(Msg, [u8; 3]); 4] = [(Msg::ColBlack, [0, 0, 0]), (Msg::ColGray, [89, 89, 89]), (Msg::ColBlue, [0, 64, 160]), (Msg::ColRed, [192, 0, 0])];
/// What the box holds when it is first opened, in the order the box's things that are filled in are in: the six places (the head's left,
/// middle and right, then the foot's), size, colour (the number of the entry), margin, first number, first and last page, and the two
/// switches (`1` ticked).
const START: [&str; 14] = ["", "", "", "", "{page}", "", "10.5", "0", "15", "1", "", "", "0", "0"];

/// The things in the box, in the order they are made in: what each is, its words (the typed ones have none: they are what was typed), and
/// where it is (x, y, width, height). The things that are filled in are in the order of [`START`]. A list that drops down is as tall as its
/// list.
#[rustfmt::skip]
const BOX: [(FormKind, Option<Msg>, [i32; 4]); 31] = [
    (Group, Some(Msg::PnHeader), [12, 6, 616, 66]), (Label, Some(Msg::PnLeft), [24, 36, 36, 18]), (Combo, None, [60, 32, 150, 150]),
    (Label, Some(Msg::PnCenter), [222, 36, 36, 18]), (Combo, None, [258, 32, 150, 150]), (Label, Some(Msg::PnRight), [420, 36, 36, 18]), (Combo, None, [456, 32, 150, 150]),
    (Group, Some(Msg::PnFooter), [12, 80, 616, 66]), (Label, Some(Msg::PnLeft), [24, 110, 36, 18]), (Combo, None, [60, 106, 150, 150]),
    (Label, Some(Msg::PnCenter), [222, 110, 36, 18]), (Combo, None, [258, 106, 150, 150]), (Label, Some(Msg::PnRight), [420, 110, 36, 18]), (Combo, None, [456, 106, 150, 150]),
    (Label, Some(Msg::PnFields), [12, 152, 616, 18]),
    (Label, Some(Msg::PnSize), [12, 184, 40, 18]), (Edit, None, [54, 180, 50, 24]), (Label, Some(Msg::PnColor), [116, 184, 40, 18]), (Pick, None, [158, 180, 84, 150]),
    (Label, Some(Msg::PnMargin), [254, 184, 110, 18]), (Edit, None, [366, 180, 44, 24]), (Label, Some(Msg::PnStart), [424, 184, 70, 18]), (Edit, None, [496, 180, 56, 24]),
    (Label, Some(Msg::PnRange), [12, 216, 90, 18]), (Edit, None, [104, 212, 50, 24]), (Label, Some(Msg::PnTo), [162, 216, 24, 18]), (Edit, None, [188, 212, 50, 24]),
    (Label, Some(Msg::PnRangeHint), [248, 216, 200, 18]),
    (Check, Some(Msg::PnMirror), [12, 246, 380, 22]), (Check, Some(Msg::PnSkipFirst), [400, 246, 220, 22]),
    (Button, Some(Msg::PnRemove), [12, 282, 280, 28]),
];

/// A number written with digits and at most one point ("10.5"), if it is one.
fn decimal(text: &str) -> Option<f32> {
    let text = text.trim();
    let (whole, part) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(part) || whole.len() + part.len() == 0 {
        return None;
    }
    Some(whole.parse::<u32>().unwrap_or(0) as f32 + part.parse::<u32>().unwrap_or(0) as f32 / 10f32.powi(part.len() as i32))
}

/// What the box holds (as typed: it is read when the box is closed), and where today's date comes from.
pub(super) struct Pagenum {
    values: Vec<String>,
    pub today: fn() -> String,
}

impl Pagenum {
    pub(super) fn new() -> Pagenum {
        Pagenum { values: START.iter().map(ToString::to_string).collect(), today: String::new }
    }
}

impl App {
    /// The program says where today's date comes from (the clock of the person's time zone).
    pub fn set_clock(&mut self, today: fn() -> String) {
        self.pn.today = today;
    }

    pub(super) fn ask_pagenum(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if !self.rights.modify {
            return self.say(t(self.lang, Msg::NoteModifyDenied).to_string());
        }
        let lang = self.lang;
        // Each thing of the box with what the person left in it the last time.
        let mut typed = self.pn.values.iter();
        let items = BOX
            .iter()
            .map(|&(kind, words, [x, y, w, h])| {
                let mut item = FormItem { kind, text: words.map_or(String::new(), |m| t(lang, m).to_string()), rect: (x, y, w, h), choices: Vec::new(), selected: 0 };
                if matches!(kind, Edit | Combo | Pick | Check) {
                    let value = typed.next().cloned().unwrap_or_default();
                    match kind {
                        Combo => (item.text, item.choices) = (value, PRESETS.iter().map(ToString::to_string).collect()),
                        Edit => item.text = value,
                        Pick => (item.selected, item.choices) = (value.parse().unwrap_or(0), COLORS.iter().map(|(m, _)| t(lang, *m).to_string()).collect()),
                        _ => item.selected = usize::from(value == "1"),
                    }
                }
                item
            })
            .collect::<Vec<FormItem>>();
        // The sketch shows the page in view: its number, the number of pages, and the shape of the page.
        let (page, count) = (self.current_page(), self.page_count());
        let shape = self.doc.as_ref().map_or((1, 1), |d| d.layout.size_px(page));
        let combos: Vec<usize> = items.iter().enumerate().filter(|(_, i)| i.kind == Combo).map(|(k, _)| k).collect();
        let preview = FormPreview {
            rect: (644, 10, 164, 232),
            shape: (shape.0 as i32, shape.1 as i32),
            slots: std::array::from_fn(|i| combos.get(i).copied().unwrap_or(0)),
            mirror: items.iter().position(|i| i.kind == Check).unwrap_or(0),
            fields: [(page + 1).to_string(), count.to_string(), (self.pn.today)(), self.file_name()],
            left_page: page % 2 == 1,
        };
        vec![Action::AskForm(FormSpec { tag: TAG_PAGENUM, title: t(lang, Msg::PnTitle).to_string(), size: (816, 318), items, ok: t(lang, Msg::Ok).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: Some(preview) })]
    }

    /// The name of the open file, without its folder.
    fn file_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The box was closed: with OK the numbers are put on the pages; with the button, what was put on before is taken away.
    pub(super) fn pagenum_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { values, button }) = answer.filter(|a| a.values.len() == START.len()) else { return Vec::new() };
        if self.doc.is_none() {
            return Vec::new();
        }
        // What was typed is kept for the next time, whatever happens to it.
        let v = |i: usize| values.get(i).map_or("", |s| s.trim());
        let (size, margin, start, color) = (decimal(v(6)), decimal(v(8)), v(9).parse::<u32>().ok().filter(|&n| n <= MAX_START), COLORS.get(v(7).parse().unwrap_or(0)));
        // The pages: from the first to the last of the list, 1-based (the whole file when both are empty).
        let count = self.page_count() as u32;
        let page = |text: &str, default: u32| if text.is_empty() { Some(default) } else { text.parse::<u32>().ok().filter(|&n| n >= 1) };
        let (from, to) = (page(v(10), 1), page(v(11), count).map(|n| n.min(count)));
        let spec = (|| {
            let (from, to, size, margin, start, color) = (from?, to?, size?, margin?, start?, color?);
            (from <= to && margin <= MAX_MARGIN_MM).then(|| Pagination {
                slots: std::array::from_fn(|i| values.get(i).cloned().unwrap_or_default()),
                size,
                color: color.1,
                margin,
                pages: (from - 1..to).collect(),
                start,
                mirror: v(12) == "1",
                skip_first: v(13) == "1",
                file: self.file_name(),
                date: (self.pn.today)(),
            })
        })();
        self.pn.values = values;
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let id = self.new_id();
        if button.is_some() {
            self.engine.remove_pagination(self.doc_id, id);
            self.edit.asked.insert(id, Asked::Unpagenum);
        } else if let Some(spec) = spec {
            self.engine.add_pagination(self.doc_id, id, spec);
            self.edit.asked.insert(id, Asked::Pagenum);
        } else {
            return self.say(t(self.lang, Msg::NotePagenumBad).to_string());
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::i18n::Lang;

    fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..1000 {
            app.event(Event::Wake);
            if until(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    /// Four pages of 300 x 300 points.
    fn four_pages() -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        let objects = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 /MediaBox [0 0 300 300] >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R >>".to_string()),
            (4, "<< /Type /Page /Parent 2 0 R >>".to_string()),
            (5, "<< /Type /Page /Parent 2 0 R >>".to_string()),
            (6, "<< /Type /Page /Parent 2 0 R >>".to_string()),
        ];
        for (n, body) in &objects {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let at = out.len();
        out.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        for n in 1..7 {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        out
    }

    /// The pages (from 0) of the saved file that have page numbers of ours.
    fn numbered(file: &std::path::Path) -> Vec<usize> {
        let doc = qingpdf_core::Document::open(file).unwrap();
        let pages = doc.pages().unwrap();
        (0..pages.len()).filter(|&i| matches!(doc.get(pages[i].obj_ref), Ok(qingpdf_core::Object::Dict(d)) if d.get("QpPagination").is_some())).collect()
    }

    fn answer(values: &[&str], button: Option<usize>) -> Event {
        Event::Form { tag: TAG_PAGENUM, answer: Some(FormAnswer { values: values.iter().map(ToString::to_string).collect(), button }) }
    }

    fn insert_menu_item_enabled(app: &App) -> bool {
        app.menus().iter().any(|m| m.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, enabled: true, .. } if *id == CMD_INS_PAGENUM)))
    }

    #[test]
    fn the_box_puts_numbers_on_the_pages_asked_for_keeps_what_was_typed_and_takes_them_away_again() {
        if !["arial.ttf", "msyh.ttc", "simsun.ttc"].iter().any(|f| std::path::Path::new("C:/Windows/Fonts").join(f).is_file()) {
            println!("SKIPPED: no TrueType font on this machine");
            return;
        }
        let dir = std::env::temp_dir().join(format!("qingpdf-view-pagenum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("four.pdf");
        std::fs::write(&file, four_pages()).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        // Nothing is open: the menu item does nothing.
        assert!(app.event(Event::Command(CMD_INS_PAGENUM)).is_empty() && !insert_menu_item_enabled(&app));
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        app.set_clock(|| "2026-10-15".to_string());
        assert!(insert_menu_item_enabled(&app));

        // The box: six places with the usual templates to pick from, the foot's middle starts as `{page}`, the options, one button.
        let asked = app.event(Event::Command(CMD_INS_PAGENUM));
        let [Action::AskForm(form)] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(form.tag, TAG_PAGENUM);
        let combos: Vec<&FormItem> = form.items.iter().filter(|i| i.kind == FormKind::Combo).collect();
        assert_eq!(combos.len(), 6);
        assert!(combos.iter().all(|c| c.choices.len() == 6 && c.choices[3] == "- {page} -"));
        assert_eq!(combos.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["", "", "", "", "{page}", ""]);
        assert_eq!(form.items.iter().filter(|i| i.kind == FormKind::Button).count(), 1);
        // Everything fits in the box (a list that drops down is as tall as its list, which may go below the foot).
        assert!(form.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= form.size.0 && (i.kind == FormKind::Combo || i.kind == FormKind::Pick || i.rect.1 + i.rect.3 <= form.size.1)));
        let inputs = form.items.iter().filter(|i| matches!(i.kind, FormKind::Edit | FormKind::Combo | FormKind::Pick | FormKind::Check)).count();
        assert_eq!(inputs, 14, "six places, size, colour, margin, start, from, to, two switches");

        // Cancelled: nothing happens. Numbers that are not numbers, or no range: the person is told and nothing is sent.
        assert!(app.event(Event::Form { tag: TAG_PAGENUM, answer: None }).is_empty() && !app.edit.dirty);
        app.event(answer(&["", "", "", "", "{page}", "", "big", "0", "15", "1", "", "", "0", "0"], None));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("Check the numbers")) && !app.edit.dirty && app.edit.asked.is_empty(), "{:?}", app.note);
        app.note = None;
        app.event(answer(&["", "", "", "", "{page}", "", "12", "0", "15", "1", "3", "2", "0", "0"], None));
        assert!(app.note.is_some() && !app.edit.dirty && app.edit.asked.is_empty(), "from 3 to 2 is no range");

        // Pages 2 to 3, the first of them numbered 5, in blue, mirrored: "Page 5 of 6" in the middle of the foot and the file's name at the head.
        app.event(answer(&["{file}", "", "", "", "Page {page} of {pages}", "- {page} -", "12", "2", "10", "5", "2", "3", "1", "0"], None));
        pump(&mut app, "the numbers", |a| a.edit.asked.is_empty());
        assert!(app.edit.dirty && app.edit.can_undo && app.note.as_deref().is_some_and(|n| n.contains("Page numbers added")), "{:?}", app.note);
        // What was typed is there the next time.
        let again = app.event(Event::Command(CMD_INS_PAGENUM));
        let [Action::AskForm(form)] = &again[..] else { panic!("{again:?}") };
        let typed: Vec<String> = form.items.iter().filter(|i| matches!(i.kind, FormKind::Edit | FormKind::Combo)).map(|i| i.text.clone()).collect();
        assert_eq!((typed[0].as_str(), typed[4].as_str(), typed[6].as_str(), typed[7].as_str(), typed[8].as_str(), typed[9].as_str(), typed[10].as_str()), ("{file}", "Page {page} of {pages}", "12", "10", "5", "2", "3"));
        assert!(form.items.iter().any(|i| i.kind == FormKind::Pick && i.selected == 2) && form.items.iter().any(|i| i.kind == FormKind::Check && i.selected == 1));
        app.event(Event::Form { tag: TAG_PAGENUM, answer: None });

        app.event(Event::Command(CMD_SAVE));
        pump(&mut app, "the save", |a| !a.edit.dirty && a.edit.saving.is_none());
        assert_eq!(numbered(&file), vec![1, 2], "pages 2 and 3 only");

        // Taking them away: the same box's button. Nothing of ours is left in the saved file.
        app.event(answer(&["{file}", "", "", "", "Page {page} of {pages}", "- {page} -", "12", "2", "10", "5", "2", "3", "1", "0"], Some(0)));
        pump(&mut app, "the removal", |a| a.edit.asked.is_empty());
        assert!(app.edit.dirty && app.note.as_deref().is_some_and(|n| n.contains("taken away")), "{:?}", app.note);
        app.event(Event::Command(CMD_SAVE));
        pump(&mut app, "the save", |a| !a.edit.dirty && a.edit.saving.is_none());
        assert_eq!(numbered(&file), Vec::<usize>::new());
        // Once more: there is nothing of ours any more, and the engine says so.
        app.event(answer(&["", "", "", "", "{page}", "", "12", "0", "15", "1", "", "", "0", "0"], Some(0)));
        pump(&mut app, "the answer", |a| a.edit.asked.is_empty());
        assert!(app.note.as_deref().is_some_and(|n| n.contains("no page numbers")), "{:?}", app.note);

        // A file whose content may not be changed: told why, no box.
        app.rights.modify = false;
        let denied = app.event(Event::Command(CMD_INS_PAGENUM));
        assert!(denied.iter().all(|a| !matches!(a, Action::AskForm(_))) && app.note.as_deref().is_some_and(|n| n.contains("content")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
