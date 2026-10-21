//! What a file says about itself (2026-10-21), for the properties box and `qingpdf info`: the version, the page count, the information dictionary,
//! the dates, how it is protected, whether it is signed, tagged or linearized, how many files are attached to it, whether it has a script, and the
//! fonts its pages use (named, with whether each is in the file). Nothing is changed; what is looked at is bounded, so a file made to be slow to look
//! at gives a list that stops early, and says so.
//!
//! The dates are told as the seconds since 1970 in UTC (and the zone the file wrote, if it wrote one), so that the window can show them in the
//! person's own time and the command line in the file's.

use std::time::Instant;

use crate::document::{Document, Page};
use crate::idmap::IdSet;
use crate::info::decode_text_string;
use crate::object::{Dict, Object};
use crate::security::Encryption;

/// A date of the file (7.9.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfDate {
    /// Seconds since 1970-01-01 00:00:00 UTC.
    pub utc: i64,
    /// Minutes east of UTC the file wrote it in (`Z` is 0); `None` when it did not say.
    pub offset: Option<i32>,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // Howard Hinnant's algorithm.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date and time of `secs` since 1970 (UTC): year, month, day, hour, minute, second.
pub fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, (rest / 3_600) as u32, (rest % 3_600 / 60) as u32, (rest % 60) as u32)
}

/// `2026-10-21 14:03:09` for `secs` since 1970 (the zone is the caller's business).
pub fn format_secs(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

impl PdfDate {
    /// The date as the file wrote it: `2026-10-21 14:03:09 +08:00`, `... UTC`, or with no zone when the file named none.
    pub fn text(&self) -> String {
        match self.offset {
            Some(0) => format!("{} UTC", format_secs(self.utc)),
            Some(m) => format!("{} {}{:02}:{:02}", format_secs(self.utc + i64::from(m) * 60), if m < 0 { '-' } else { '+' }, m.abs() / 60, m.abs() % 60),
            None => format_secs(self.utc),
        }
    }
}

/// A date string of the file: `D:YYYYMMDDHHmmSSOHH'mm'`, everything after the year optional (the prefix `D:` too, which some programs leave out).
pub fn parse_date(s: &str) -> Option<PdfDate> {
    // (Some programs write the date with the parentheses of a string in the string itself.)
    let s = s.trim().trim_start_matches('(').trim_end_matches(')');
    let s = s.strip_prefix("D:").unwrap_or(s);
    let used = s.bytes().take_while(u8::is_ascii_digit).count().min(14);
    if used < 4 || used % 2 != 0 {
        return None;
    }
    let (date, zone) = s.split_at(used);
    let part = |at: usize, default: i64| date.get(at..at + 2).map_or(Some(default), |t| t.parse::<i64>().ok());
    let year = date.get(..4)?.parse::<i64>().ok()?;
    let (mo, d, h, mi, sec) = (part(4, 1)?, part(6, 1)?, part(8, 0)?, part(10, 0)?, part(12, 0)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 24 || mi > 59 || sec > 60 {
        return None;
    }
    let offset = match zone.as_bytes().first() {
        None => None,
        Some(b'Z' | b'z') => Some(0),
        Some(&sign @ (b'+' | b'-')) => {
            // `+0800`, `+08'00'`, `+08`: the digits, two for the hours and two for the minutes.
            let v: Vec<i32> = zone.bytes().filter(u8::is_ascii_digit).map(|b| i32::from(b - b'0')).collect();
            let at = |i: usize| v.get(i).copied().unwrap_or(0);
            let (hh, mm) = (at(0) * 10 + at(1), at(2) * 10 + at(3));
            if hh > 23 || mm > 59 {
                return None;
            }
            Some(if sign == b'-' { -(hh * 60 + mm) } else { hh * 60 + mm })
        }
        Some(_) => return None,
    };
    let local = days_from_civil(year, mo, d) * 86_400 + h * 3_600 + mi * 60 + sec;
    Some(PdfDate { utc: local - i64::from(offset.unwrap_or(0)) * 60, offset })
}

/// Is a font in the file?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Embedded {
    No,
    Yes,
    /// Only the characters of the file (the name starts with six capitals and a plus sign, 9.6.4).
    Subset,
}

/// A font the pages use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontInfo {
    /// The name without the prefix of a subset.
    pub name: String,
    /// `Type1`, `TrueType`, `Type3`, `MMType1`, or `Type0` with the kind of its descendant (`Type0/CIDFontType2`).
    pub kind: String,
    pub embedded: Embedded,
}

/// What looking for the fonts may do: how many it lists, how many objects it reads, how many bytes of the forms it reads, and the time it takes.
#[derive(Clone, Copy, Debug)]
pub struct FontLimits {
    pub fonts: usize,
    pub steps: usize,
    pub bytes: usize,
    pub deadline: Option<Instant>,
}

impl FontLimits {
    /// What the properties box gives it: a list of 300 and some tens of milliseconds.
    pub fn for_the_box() -> FontLimits {
        FontLimits { fonts: 300, steps: 40_000, bytes: 32 << 20, deadline: Some(Instant::now() + std::time::Duration::from_millis(40)) }
    }

    /// What the command line gives it: no time limit.
    pub fn for_the_command_line() -> FontLimits {
        FontLimits { fonts: 2_000, steps: 2_000_000, bytes: 512 << 20, deadline: None }
    }
}

struct Fonts<'a> {
    doc: &'a Document,
    lim: FontLimits,
    out: Vec<FontInfo>,
    seen_fonts: IdSet<u32>,
    seen_resources: IdSet<u32>,
    steps: usize,
    bytes: usize,
    cut: bool,
}

impl Fonts<'_> {
    /// One more object read; false when a limit is reached (the list is then cut).
    fn step(&mut self) -> bool {
        self.steps += 1;
        if self.cut || self.steps > self.lim.steps || self.bytes > self.lim.bytes || (self.steps.is_multiple_of(64) && self.lim.deadline.is_some_and(|t| Instant::now() > t)) {
            self.cut = true;
        }
        !self.cut
    }

    fn dict(&mut self, o: &Object) -> Option<Dict> {
        if !self.step() {
            return None;
        }
        match self.doc.resolve(o).ok()? {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => {
                self.bytes = self.bytes.saturating_add(s.data.len());
                Some(s.dict)
            }
            _ => None,
        }
    }

    fn resources(&mut self, res: &Object, depth: usize) {
        if let Object::Ref(r) = res
            && !self.seen_resources.insert(r.num)
        {
            return;
        }
        let Some(d) = self.dict(res) else { return };
        if let Some(fonts) = d.get("Font").and_then(|f| self.dict(f)) {
            for (_, f) in fonts.iter() {
                self.font(f);
            }
        }
        if depth < 3
            && let Some(xobjects) = d.get("XObject").and_then(|x| self.dict(x))
        {
            for (_, x) in xobjects.iter() {
                if self.cut {
                    return;
                }
                if let Object::Ref(r) = x
                    && !self.seen_resources.insert(r.num)
                {
                    continue;
                }
                if let Some(form) = self.dict(x).filter(|f| f.get_name("Subtype").is_some_and(|s| s == "Form"))
                    && let Some(inner) = form.get("Resources")
                {
                    self.resources(inner, depth + 1);
                }
            }
        }
    }

    fn font(&mut self, f: &Object) {
        if let Object::Ref(r) = f
            && !self.seen_fonts.insert(r.num)
        {
            return;
        }
        if self.out.len() >= self.lim.fonts {
            self.cut = true;
            return;
        }
        let Some(d) = self.dict(f) else { return };
        let subtype = d.get_name("Subtype").map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned()).unwrap_or_default();
        let base = d.get_name("BaseFont").map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned()).unwrap_or_default();
        // A composite font's descriptor is its descendant's.
        let (kind, descriptor_holder) = if subtype == "Type0" {
            let inner = d.get("DescendantFonts").and_then(|a| self.dict_array_first(a));
            let sub = inner.as_ref().and_then(|i| i.get_name("Subtype")).map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned()).unwrap_or_default();
            (format!("Type0/{sub}"), inner)
        } else {
            (subtype.clone(), Some(d.clone()))
        };
        let has_file = |this: &mut Self, holder: &Option<Dict>| {
            holder.as_ref().and_then(|h| h.get("FontDescriptor")).and_then(|fd| this.dict(fd)).is_some_and(|fd| ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| fd.contains_key(k)))
        };
        let in_file = subtype == "Type3" || has_file(self, &descriptor_holder);
        let (name, subset) = match base.split_once('+') {
            Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => (rest.to_string(), true),
            _ => (base, false),
        };
        let embedded = match (in_file, subset) {
            (false, _) => Embedded::No,
            (true, true) => Embedded::Subset,
            (true, false) => Embedded::Yes,
        };
        // (Several font objects with the same name are one line.)
        let info = FontInfo { name: plain(&name, MAX_FONT_NAME), kind: plain(&kind, MAX_FONT_NAME), embedded };
        if !self.out.contains(&info) {
            self.out.push(info);
        }
    }

    fn dict_array_first(&mut self, a: &Object) -> Option<Dict> {
        let arr = self.doc.resolve(a).ok()?;
        let first = arr.as_array()?.first()?.clone();
        self.dict(&first)
    }
}

/// The fonts the pages use, once each in the order they are met, and whether the list was cut short by a limit.
#[inline(never)]
pub fn fonts(doc: &Document, pages: &[Page], lim: FontLimits) -> (Vec<FontInfo>, bool) {
    let mut scan = Fonts { doc, lim, out: Vec::new(), seen_fonts: IdSet::new(), seen_resources: IdSet::new(), steps: 0, bytes: 0, cut: false };
    for page in pages {
        if scan.cut {
            break;
        }
        if let Some(res) = page.resources() {
            scan.resources(res, 0);
        }
    }
    (scan.out, scan.cut)
}

/// What the properties box shows of a file.
#[derive(Clone, Debug)]
pub struct Properties {
    pub version: (u8, u8),
    pub pages: usize,
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
    pub producer: String,
    pub created: Option<PdfDate>,
    pub modified: Option<PdfDate>,
    pub encryption: Option<Encryption>,
    pub signed: bool,
    pub tagged: bool,
    pub linearized: bool,
    /// Files attached to the file, and scripts in it; `counted_all` is false when the count stopped at a limit (there may be more).
    pub attachments: usize,
    pub scripts: usize,
    pub counted_all: bool,
    /// The file has an XMP metadata stream (a change to the properties changes it too).
    pub xmp: bool,
    pub fonts: Vec<FontInfo>,
    pub fonts_cut: bool,
}

/// The most characters of the creator and the producer (the four that can be changed have `edit::MAX_META_CHARS`).
pub const MAX_ABOUT_CHARS: usize = 200;
/// The most characters of the name of a font.
const MAX_FONT_NAME: usize = 100;

/// A text of a file made safe to show in one line of a box: a control character (a line break, a bell, ...) or one that turns the text round
/// (U+202E and its kind) is a space, and it is cut to `max` characters.
pub fn plain(text: &str, max: usize) -> String {
    text.chars().take(max).map(|c| if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') { ' ' } else { c }).collect()
}

fn text_of(doc: &Document, info: &Dict, key: &str, max: usize) -> String {
    match info.get(key).and_then(|v| doc.resolve(v).ok()) {
        Some(Object::String(s)) => plain(&decode_text_string(s.bytes.get(..s.bytes.len().min(8_000)).unwrap_or(&[])), max),
        _ => String::new(),
    }
}

fn date_of(doc: &Document, info: &Dict, key: &str) -> Option<PdfDate> {
    match info.get(key).and_then(|v| doc.resolve(v).ok()) {
        Some(Object::String(s)) => parse_date(&String::from_utf8_lossy(s.bytes.get(..s.bytes.len().min(64)).unwrap_or(&[]))),
        _ => None,
    }
}

/// Is the file linearized (Annex F): its first object, near the start of the file, is the linearization dictionary?
#[inline(never)]
fn is_linearized(doc: &Document) -> bool {
    let head = doc.data().get(..doc.data().len().min(2_048)).unwrap_or(&[]);
    head.windows(11).any(|w| w == b"/Linearized")
}

/// Gather the properties of `doc` (the pages are the page list the caller has).
#[inline(never)]
pub fn read(doc: &Document, pages: &[Page], lim: Option<FontLimits>) -> Properties {
    let info = doc.info().ok().flatten().unwrap_or_default();
    let catalog = doc.catalog().unwrap_or_default();
    let marked = catalog.get("MarkInfo").and_then(|m| doc.resolve(m).ok()).is_some_and(|m| matches!(m.as_dict().and_then(|d| d.get("Marked")), Some(Object::Bool(true))));
    let (attachments, scripts, counted_all) = crate::edit::count_active(doc, pages, 60_000);
    let (fonts, fonts_cut) = lim.map_or((Vec::new(), false), |lim| fonts(doc, pages, lim));
    Properties {
        version: doc.version(),
        pages: pages.len(),
        title: text_of(doc, &info, "Title", crate::edit::MAX_META_CHARS),
        author: text_of(doc, &info, "Author", crate::edit::MAX_META_CHARS),
        subject: text_of(doc, &info, "Subject", crate::edit::MAX_META_CHARS),
        keywords: text_of(doc, &info, "Keywords", crate::edit::MAX_META_CHARS),
        creator: text_of(doc, &info, "Creator", MAX_ABOUT_CHARS),
        producer: text_of(doc, &info, "Producer", MAX_ABOUT_CHARS),
        created: date_of(doc, &info, "CreationDate"),
        modified: date_of(doc, &info, "ModDate"),
        encryption: doc.encryption(),
        signed: crate::edit::has_signature(doc),
        tagged: marked || catalog.contains_key("StructTreeRoot"),
        linearized: is_linearized(doc),
        attachments,
        scripts,
        counted_all,
        xmp: matches!(catalog.get("Metadata"), Some(Object::Ref(_))),
        fonts,
        fonts_cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    #[test]
    fn dates_are_read_in_every_form_the_files_have_them() {
        let d = |s: &str| parse_date(s);
        assert_eq!(d("D:20260101000000Z"), Some(PdfDate { utc: 1_767_225_600, offset: Some(0) }));
        assert_eq!(d("D:20260101080000+08'00'"), Some(PdfDate { utc: 1_767_225_600, offset: Some(480) }));
        assert_eq!(d("D:20251231190000-05'00'"), Some(PdfDate { utc: 1_767_225_600, offset: Some(-300) }));
        assert_eq!(d("D:202601010800+0800"), Some(PdfDate { utc: 1_767_225_600, offset: Some(480) }));
        assert_eq!(d("20260101080000+08'00"), Some(PdfDate { utc: 1_767_225_600, offset: Some(480) }));
        assert_eq!(d("D:20260101000000"), Some(PdfDate { utc: 1_767_225_600, offset: None }));
        assert_eq!(d("(D:20260101080000+08'00')"), Some(PdfDate { utc: 1_767_225_600, offset: Some(480) }));
        assert_eq!(d("D:2026"), Some(PdfDate { utc: 1_767_225_600, offset: None }));
        assert_eq!(d("D:202602"), Some(PdfDate { utc: 1_769_904_000, offset: None }));
        assert_eq!(d("D:20000229120000Z").map(|x| x.utc), Some(951_825_600));
        assert_eq!(d("D:19700101000000Z").map(|x| x.utc), Some(0));
        assert_eq!(d("D:19691231235959Z").map(|x| x.utc), Some(-1));
        for bad in ["", "D:", "yesterday", "D:20261301000000Z", "D:20260132000000Z", "D:20260101250000Z", "D:2026010100006", "D:20260101000000+99'00'", "D:20260101000000Q", "D:20260101000000+0860"] {
            assert_eq!(d(bad), None, "{bad}");
        }
        // Told back: in the file's zone, or UTC.
        assert_eq!(d("D:20260101080000+08'00'").unwrap().text(), "2026-01-01 08:00:00 +08:00");
        assert_eq!(d("D:20251231190000-05'30'").unwrap().text(), "2025-12-31 19:00:00 -05:30");
        assert_eq!(d("D:20260101000000Z").unwrap().text(), "2026-01-01 00:00:00 UTC");
        assert_eq!(d("D:20260101000000").unwrap().text(), "2026-01-01 00:00:00");
        assert_eq!(civil(951_825_600), (2000, 2, 29, 12, 0, 0));
        assert_eq!(civil(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn what_the_file_says_about_itself_is_made_one_plain_and_short_line() {
        // A text string in UTF-16 with a line break, a bell and a right-to-left override in it, five thousand characters long.
        let hex = |s: &str| format!("<FEFF{}>", s.encode_utf16().map(|u| format!("{u:04X}")).collect::<String>());
        let evil = "Line one\nLine two\u{7}\u{202E}gnp.exe\r\n".repeat(150);
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 5 0 R >> >> >>");
        b.obj(4, &format!("<< /Creator {} /Producer {} /Title {} /Author (a\\nb) >>", hex(&evil), hex(&evil), hex(&evil)));
        b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Evil#0AName#0D#07 >>");
        let doc = Document::from_bytes(b.finish_classic(20, "/Root 1 0 R /Info 4 0 R")).unwrap();
        let p = read(&doc, &doc.pages().unwrap(), Some(FontLimits::for_the_command_line()));
        let plain_text = |s: &str| s.chars().all(|c| !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2028}' | '\u{2029}'));
        assert!(p.creator.chars().count() <= MAX_ABOUT_CHARS && p.producer.chars().count() <= MAX_ABOUT_CHARS && p.creator.starts_with("Line one Line two"), "{:?}", p.creator);
        assert!(plain_text(&p.creator) && plain_text(&p.producer) && plain_text(&p.title) && plain_text(&p.author), "{:?}", p.creator);
        // The four that can be changed keep what they can have.
        assert_eq!(p.title.chars().count(), crate::edit::MAX_META_CHARS);
        assert_eq!(p.author, "a b");
        assert_eq!(p.fonts.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Evil Name  "]);
        assert_eq!(plain("a\tb\u{85}c\u{2066}d", 10), "a b c d");
        assert_eq!(plain("abcdef", 3), "abc");
    }

    #[test]
    fn the_fonts_are_listed_once_with_whether_they_are_in_the_file() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 8 0 R >> /XObject << /X 12 0 R >> >> >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 5 0 R /F4 9 0 R >> >> >>");
        b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.obj(6, "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Arial /FontDescriptor 7 0 R >>");
        b.obj(7, "<< /Type /FontDescriptor /FontName /ABCDEF+Arial /FontFile2 11 0 R >>");
        b.obj(8, "<< /Type /Font /Subtype /Type0 /BaseFont /SimSun /DescendantFonts [10 0 R] /Encoding /Identity-H >>");
        b.obj(9, "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << >> >>");
        b.obj(10, "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /SimSun /FontDescriptor 13 0 R >>");
        b.stream_obj(11, "", b"not really a font");
        b.stream_obj(12, "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /Font << /F5 14 0 R >> >>", b"");
        b.obj(13, "<< /Type /FontDescriptor /FontName /SimSun /Flags 4 >>");
        b.obj(14, "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /FontDescriptor 15 0 R >>");
        b.obj(15, "<< /Type /FontDescriptor /FontName /Times-Roman /FontFile 11 0 R >>");
        let doc = Document::from_bytes(b.finish_classic(20, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        let (list, cut) = fonts(&doc, &pages, FontLimits::for_the_command_line());
        assert!(!cut);
        let got: Vec<(&str, &str, Embedded)> = list.iter().map(|f| (f.name.as_str(), f.kind.as_str(), f.embedded)).collect();
        assert_eq!(
            got,
            vec![
                ("Helvetica", "Type1", Embedded::No),
                ("Arial", "TrueType", Embedded::Subset),
                ("SimSun", "Type0/CIDFontType2", Embedded::No),
                ("Times-Roman", "Type1", Embedded::Yes),
                ("", "Type3", Embedded::Yes),
            ]
        );
        // The limits cut the list and say so.
        let (list, cut) = fonts(&doc, &pages, FontLimits { fonts: 2, ..FontLimits::for_the_command_line() });
        assert!(cut && list.len() == 2);
        let (_, cut) = fonts(&doc, &pages, FontLimits { steps: 5, ..FontLimits::for_the_command_line() });
        assert!(cut);
        let (_, cut) = fonts(&doc, &pages, FontLimits { deadline: Some(Instant::now() - std::time::Duration::from_secs(1)), steps: 100_000, ..FontLimits::for_the_command_line() });
        assert!(cut || list.len() <= 2, "a deadline in the past cuts the list at the next check");
    }

    #[test]
    fn a_resource_dictionary_that_leads_to_itself_does_not_hang_the_scan() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 4 0 R >>");
        b.stream_obj(4, "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources 4 0 R /Font << /F 5 0 R >> /XObject << /Self 4 0 R >>", b"");
        b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
        let doc = Document::from_bytes(b.finish_classic(20, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        let (list, _) = fonts(&doc, &pages, FontLimits::for_the_command_line());
        assert_eq!(list.len(), 1);
    }
}
