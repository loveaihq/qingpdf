//! File > Document properties... (Ctrl+D): one box with what the file says about itself (its place and size, the version, the pages, who made it and
//! with what, the dates in the person's time zone, how it is protected, whether it is signed, tagged or linearized, how many files are attached,
//! whether it has scripts, the fonts) and the title, author, subject and keywords to change. The engine gathers it (a quick request) and makes the
//! change, in the information dictionary and in the XMP, and keeps the history (`docs/view-api.md`, "文档属性").

use qingpdf_core::security::Permissions;
use qingpdf_core::view::{Embedded, Meta, Properties};

use super::editing::Asked;
use super::*;
use crate::i18n::fill;
use crate::ui::{FormAnswer, FormItem, FormKind::*, FormSpec};

/// What the box was made from, kept for telling what the person changed.
pub(super) struct PropsWork {
    /// The request for the properties.
    asked: Option<u64>,
    /// The values of the four text lines as the box started.
    pub(super) shown: Option<[String; 4]>,
}

impl PropsWork {
    pub(super) fn new() -> PropsWork {
        PropsWork { asked: None, shown: None }
    }
}

/// The time in UTC, when nothing knows the person's time zone (a test).
pub(super) fn utc_time(secs: i64) -> String {
    format!("{} UTC", qingpdf_core::props::format_secs(secs))
}

/// A size in points as two numbers in a unit (`scale` of a point), with `decimals` at the most and no trailing zero: `210 x 297`.
fn pair((w, h): (f64, f64), scale: f64, decimals: usize) -> String {
    let one = |v: f64| {
        let s = format!("{:.decimals$}", v * scale);
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
    };
    format!("{} x {}", one(w), one(h))
}

/// How wide a text is in cells of the box (a Chinese character is two cells wide).
fn cells(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// How many lines of the box a text takes (a line that is too long for it goes on to another).
fn height_of(text: &str) -> i32 {
    16 * text.split('\n').map(|l| cells(l).div_ceil(72).max(1)).sum::<usize>() as i32 + 2
}

/// A text cut to `room` cells with `...` at the end when it is longer, so that what the file says about itself stays a line of the box.
fn clip(text: &str, room: usize) -> String {
    if cells(text) <= room {
        return text.to_string();
    }
    let mut left = room.saturating_sub(3);
    let mut out = String::new();
    for c in text.chars() {
        let w = cells(c.encode_utf8(&mut [0; 4]));
        if w > left {
            break;
        }
        left -= w;
        out.push(c);
    }
    out.push_str("...");
    out
}

/// A folder name that fits a line of the box: a long one keeps its start and its end.
fn short_path(path: &str) -> String {
    if cells(path) <= 56 {
        return path.to_string();
    }
    // The characters from one end that fit in `room` cells.
    let edge = |chars: &mut dyn Iterator<Item = char>, mut room: usize| -> Vec<char> {
        chars.take_while(|c| cells(c.encode_utf8(&mut [0; 4])) <= room && { room -= cells(c.encode_utf8(&mut [0; 4])); true }).collect()
    };
    let head: String = edge(&mut path.chars(), 12).into_iter().collect();
    let tail: String = edge(&mut path.chars().rev(), 38).into_iter().rev().collect();
    format!("{head}...{tail}")
}

/// A block of lines at `y`, which moves down below it.
fn block(items: &mut Vec<FormItem>, text: &str, y: &mut i32) {
    let h = height_of(text);
    items.push(FormItem::new(Label, text, (12, *y, 496, h)));
    *y += h + 4;
}

impl App {
    /// The program says how a time (seconds since 1970, UTC) is written in the person's time zone.
    pub fn set_local_time(&mut self, f: fn(i64) -> String) {
        self.local_time = f;
    }

    /// File > Document properties...
    #[inline(never)]
    pub(super) fn ask_properties(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let id = self.new_id();
        self.engine.properties(self.doc_id, id);
        self.pp.asked = Some(id);
        Vec::new()
    }

    /// The engine has gathered the properties: the box opens.
    #[inline(never)]
    pub(super) fn on_properties(&mut self, id: u64, p: &Properties) -> Vec<Action> {
        if self.pp.asked.take() != Some(id) || self.doc.is_none() {
            return Vec::new();
        }
        self.pp.shown = Some([p.title.clone(), p.author.clone(), p.subject.clone(), p.keywords.clone()]);
        vec![Action::AskForm(self.props_form(p))]
    }

    /// The blocks of text of the box: the file, the document, the dates, the protection, what the file is.
    #[inline(never)]
    fn props_blocks(&self, p: &Properties) -> Vec<String> {
        let lang = self.lang;
        let yes = |b: bool| t(lang, if b { Msg::PropYes } else { Msg::PropNo });
        let dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        let path = self.path.clone().unwrap_or_default();
        let text_of = |p: Option<&std::path::Path>| p.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        let page = self.thumb_current();
        let size_pt = self.doc.as_ref().and_then(|d| d.layout.page_size_pt(page)).unwrap_or((0.0, 0.0));
        let date = |d: Option<qingpdf_core::view::PdfDate>| d.map_or_else(|| "-".to_string(), |d| (self.local_time)(d.utc));
        let more = if p.counted_all { "" } else { "+" };
        let mut protection = match &p.encryption {
            Some(e) => fill(t(lang, Msg::PtCipher), &[("c", &e.method_name())]),
            None => t(lang, Msg::PtNone).to_string(),
        };
        if let Some(e) = p.encryption.as_ref().filter(|e| !e.permissions.allows_everything()) {
            let q = &e.permissions;
            let inverse = Permissions { print: !q.print, print_high_quality: !q.print_high_quality, modify: !q.modify, copy: !q.copy, annotate: !q.annotate, fill_forms: !q.fill_forms, assemble: !q.assemble, accessibility: !q.accessibility };
            protection = format!("{protection}\n{}\n{}", fill(t(lang, Msg::PropAllowed), &[("list", &self.denied_list(&inverse))]), fill(t(lang, Msg::PtDenied), &[("list", &self.denied_list(q))]));
        }
        vec![
            fill(t(lang, Msg::PropFile), &[("name", &clip(&text_of(path.file_name().map(std::path::Path::new)), 56)), ("dir", &short_path(&text_of(path.parent()))), ("size", &super::compress::size_text(size))]),
            fill(
                t(lang, Msg::PropDoc),
                &[
                    ("ver", &format!("{}.{}", p.version.0, p.version.1)),
                    ("n", &p.pages.to_string()),
                    ("p", &(page + 1).to_string()),
                    ("mm", &pair(size_pt, 25.4 / 72.0, 1)),
                    ("inch", &pair(size_pt, 1.0 / 72.0, 2)),
                    ("creator", &dash(&clip(&p.creator, 60))),
                    ("producer", &dash(&clip(&p.producer, 60))),
                ],
            ),
            fill(t(lang, Msg::PropDates), &[("created", &date(p.created)), ("modified", &date(p.modified))]),
            protection,
            fill(
                t(lang, Msg::PropFlags),
                &[
                    ("s", yes(p.signed)),
                    ("t", yes(p.tagged)),
                    ("l", yes(p.linearized)),
                    ("a", &format!("{}{more}", p.attachments)),
                    ("j", &format!("{}{more}", yes(p.scripts > 0))),
                    ("x", yes(p.xmp)),
                ],
            ),
        ]
    }

    #[inline(never)]
    fn props_form(&self, p: &Properties) -> FormSpec {
        let lang = self.lang;
        let dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        let fonts: Vec<String> = p
            .fonts
            .iter()
            .map(|f| {
                let how = match f.embedded {
                    Embedded::No => Msg::PropNotEmbedded,
                    Embedded::Yes => Msg::PropEmbedded,
                    Embedded::Subset => Msg::PropSubset,
                };
                format!("{}  [{}, {}]", dash(&f.name), f.kind, t(lang, how))
            })
            .chain(p.fonts_cut.then(|| t(lang, Msg::PropCut).to_string()))
            .collect();
        let editable = self.rights.modify && !p.signed;
        let mut items: Vec<FormItem> = Vec::new();
        let mut y = 8;
        let blocks = self.props_blocks(p);
        for text in blocks.iter().take(2) {
            block(&mut items, text, &mut y);
        }
        let names = [Msg::PropTitleLabel, Msg::PropAuthor, Msg::PropSubject, Msg::PropKeywords];
        for (name, value) in names.iter().zip([&p.title, &p.author, &p.subject, &p.keywords]) {
            items.push(FormItem::new(Label, t(lang, *name), (12, y + 3, 76, 18)));
            items.push(if editable { FormItem::new(Edit, value, (90, y, 418, 22)) } else { FormItem::new(Label, &dash(value), (90, y + 3, 418, 18)) });
            y += 26;
        }
        y += 4;
        for text in blocks.iter().skip(2) {
            block(&mut items, text, &mut y);
        }
        items.push(FormItem::new(Label, &fill(t(lang, Msg::PropFonts), &[("n", &p.fonts.len().to_string())]), (12, y + 3, 200, 18)));
        items.push(FormItem::pick(fonts, 0, (214, y, 294, 200)));
        y += 30;
        if !editable {
            items.push(FormItem::new(Label, t(lang, Msg::PropNoEdit), (12, y, 496, 18)));
            y += 22;
        }
        FormSpec { tag: TAG_PROPS, title: t(lang, Msg::PropTitle).to_string(), size: (520, y + 44), items, ok: t(lang, Msg::Ok).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None }
    }

    /// The box was closed: with OK, what was changed in the four lines is asked of the engine.
    #[inline(never)]
    pub(super) fn props_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(shown) = self.pp.shown.take() else { return Vec::new() };
        let Some(FormAnswer { values, .. }) = answer else { return Vec::new() };
        // (The four lines and the list of fonts when the file allows changing them; the list alone when it does not.)
        if values.len() != 5 || self.doc.is_none() || !self.rights.modify {
            return Vec::new();
        }
        let changed = |i: usize| values.get(i).filter(|v| shown.get(i) != Some(*v)).cloned();
        let meta = Meta { title: changed(0), author: changed(1), subject: changed(2), keywords: changed(3) };
        if meta == Meta::default() {
            return Vec::new();
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let id = self.new_id();
        self.engine.set_properties(self.doc_id, id, meta);
        self.edit.asked.insert(id, Asked::Meta);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_folder_name_keeps_its_start_and_its_end() {
        assert_eq!(short_path(r"C:\Users\me\Documents"), r"C:\Users\me\Documents");
        let long = format!(r"C:\Users\Vincent\AppData\Local\Temp\{}\reports\2026", "x".repeat(80));
        let short = short_path(&long);
        assert!(short.starts_with(r"C:\Users\Vin...") && short.ends_with(r"\reports\2026"), "{short}");
        assert!(cells(&short) <= 56);
        // (Chinese characters take two cells each.)
        let chinese = format!(r"D:\{}", "文档".repeat(40));
        let short = short_path(&chinese);
        assert!(cells(&short) <= 56 && short.starts_with(r"D:\文档") && short.contains("..."), "{short}");
        assert_eq!(pair((595.0, 842.0), 25.4 / 72.0, 1), "209.9 x 297");
        // A line of the box stays a line: what the file says about itself is cut, in cells (a Chinese character is two).
        assert_eq!(clip("Word", 60), "Word");
        let long = clip(&"x".repeat(500), 60);
        assert!(long.ends_with("...") && cells(&long) <= 60, "{long}");
        let chinese = clip(&"\u{4E2D}".repeat(100), 60);
        assert!(chinese.ends_with("...") && cells(&chinese) <= 60 && chinese.starts_with('\u{4E2D}'), "{chinese}");
    }
}
