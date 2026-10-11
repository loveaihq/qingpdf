//! Step 4b through the engine's messages: a Chinese form filled in, a text box and a picture stamped on it, saved as an incremental
//! update; the rights and the signatures that decide whether it may be done; the speed of a form of 500 fields. Every saved file is
//! checked as in `editing.rs` (original bytes a prefix, `qpdf --check`, reopened). The files these tests leave in the folder named
//! by `QINGPDF_FORMS_OUT` (default `tests/out/forms`) are drawn by PDFium and MuPDF in `tests/tools/forms_compare.py`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qingpdf_core::Document;
use qingpdf_core::object::Object;
use qingpdf_core::view::{EditResult, EditStatus, Engine, Event, FieldInfo, FieldKind, FillValue, Opened, RenderRequest, Rendered, SaveStatus, TOTAL_BYTES};

struct Viewer {
    engine: Engine,
    opened: Opened,
    next: u64,
}

impl Viewer {
    fn open(path: &Path, password: &str) -> Viewer {
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &path.to_string_lossy(), password, TOTAL_BYTES, 1920 * 1080);
        match engine.wait(20_000) {
            Some(Event::Opened(opened)) => Viewer { engine, opened, next: 100 },
            other => panic!("{}: not opened: {other:?}", path.display()),
        }
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn until<T>(&self, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        loop {
            match self.engine.wait(30_000) {
                Some(e) => {
                    if let Some(t) = pick(e) {
                        return t;
                    }
                }
                None => panic!("the engine said nothing for thirty seconds"),
            }
        }
    }

    fn edited(&self, id: u64) -> EditResult {
        self.until(|e| match e {
            Event::Edited(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn fields(&mut self, page: u32) -> Vec<FieldInfo> {
        let id = self.id();
        self.engine.fields(1, id, page);
        self.until(|e| match e {
            Event::Fields { id: i, items, .. } if i == id => Some(items),
            _ => None,
        })
    }

    /// The options of a combo box or list, asked for on their own (the list of fields does not carry them).
    fn options(&mut self, page: u32, f: &FieldInfo) -> (Vec<(String, String)>, bool) {
        let id = self.id();
        self.engine.options(1, id, page, f.index, f.num);
        self.until(|e| match e {
            Event::Options { id: i, items, truncated, .. } if i == id => Some((items, truncated)),
            _ => None,
        })
    }

    fn render(&mut self, page: u32) -> Rendered {
        let id = self.id();
        self.engine.render(RenderRequest { doc: 1, id, page, dpi: 72.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
        self.until(|e| match e {
            Event::Rendered(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    fn fill(&mut self, page: u32, f: &FieldInfo, value: FillValue) -> EditResult {
        let id = self.id();
        self.engine.fill_field(1, id, page, f.index, f.num, value);
        self.edited(id)
    }

    fn text(&mut self, page: u32, f: &FieldInfo, value: &str) -> EditResult {
        self.fill(page, f, FillValue::Text(value.to_string()))
    }

    fn freetext(&mut self, page: u32, rect: [f32; 4], text: &str, size: f32, color: [u8; 3]) -> EditResult {
        let id = self.id();
        self.engine.add_freetext(1, id, page, rect, text, size, color, "Tester");
        self.edited(id)
    }

    fn stamp(&mut self, page: u32, rect: [f32; 4], image: Vec<u8>) -> EditResult {
        let id = self.id();
        self.engine.add_stamp(1, id, page, rect, image, "Tester");
        self.edited(id)
    }

    fn save(&mut self, path: &Path) -> (SaveStatus, String, u64) {
        let id = self.id();
        self.engine.save(1, id, &path.to_string_lossy());
        self.until(|e| match e {
            Event::Saved { id: i, status, message, bytes, .. } if i == id => Some((status, message, bytes)),
            _ => None,
        })
    }
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("QINGPDF_FORMS_OUT").map(PathBuf::from).unwrap_or_else(|| common::workspace_root().join("tests").join("out").join("forms"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file built by hand: the objects (number and text) and the trailer's extra entries.
fn pdf(objects: &[(u32, String)], trailer: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|o| o.0).max().unwrap() + 1;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None if n == 0 => out.extend_from_slice(b"0000000000 65535 f \n"),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R {trailer} >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

fn hex16(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        out.push_str(&format!("{u:04X}"));
    }
    out.push('>');
    out
}

/// A one-page Chinese form: name, address (several lines), postcode (comb of six), city (combo box with Chinese options), a check
/// box, a radio group of two and a list. The page has the labels in Helvetica. None of the fields has an appearance.
fn chinese_form(catalog_extra: &str, page_extra: &str) -> Vec<u8> {
    let city = format!("[{} {} {}]", hex16("北京"), hex16("上海"), hex16("广州"));
    let list = format!("[{} {} {}]", hex16("苹果"), hex16("香蕉"), hex16("橘子"));
    let labels = "BT /Helv 10 Tf 20 368 Td (Name) Tj 0 -90 Td (Address) Tj 0 -50 Td (Postcode) Tj 0 -50 Td (City) Tj 0 -50 Td (Agree) Tj 70 0 Td (F) Tj 40 0 Td (M) Tj -110 -40 Td (Fruit) Tj ET";
    let objects = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 18 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 30 0 R >> >> >> {catalog_extra} >>")),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Resources << /Font << /Helv 30 0 R >> >> /Contents 4 0 R /Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 16 0 R 17 0 R 18 0 R] {page_extra} >>")),
        (4, format!("<< /Length {} >>\nstream\n{labels}\nendstream", labels.len())),
        (10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [20 340 280 365] /MK << /BC [0 0 0] /BG [1 1 0.8] >> /DA (/Helv 0 Tf 0 0 0.6 rg) >>".to_string()),
        (11, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (address) /Ff 4096 /Rect [20 250 280 330] /MK << /BC [0.5 0.5 0.5] >> /DA (/Helv 12 Tf 0 g) >>".to_string()),
        (12, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (code) /Ff 16777216 /MaxLen 6 /Q 1 /Rect [20 200 200 225] /MK << /BC [0 0 0] >> >>".to_string()),
        (13, format!("<< /Type /Annot /Subtype /Widget /FT /Ch /T (city) /Ff 131072 /Rect [20 150 200 175] /MK << /BC [0 0 0] >> /Opt {city} >>")),
        (14, "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [20 100 40 120] /MK << /BC [0 0 0] /CA (4) >> >>".to_string()),
        (15, "<< /FT /Btn /Ff 32768 /T (gender) /Kids [16 0 R 17 0 R] >>".to_string()),
        (16, "<< /Type /Annot /Subtype /Widget /Parent 15 0 R /Rect [90 100 110 120] /MK << /BC [0 0 0] >> >>".to_string()),
        (17, "<< /Type /Annot /Subtype /Widget /Parent 15 0 R /Rect [130 100 150 120] /MK << /BC [0 0 0] >> >>".to_string()),
        (18, format!("<< /Type /Annot /Subtype /Widget /FT /Ch /T (fruit) /Rect [20 20 200 70] /MK << /BC [0 0 0] >> /Opt {list} >>")),
        (30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string()),
    ];
    pdf(&objects, "")
}

/// A PNG of 96 by 48 pixels: a red ring on a transparent ground, the middle half see-through, the rest opaque.
fn signature_png() -> Vec<u8> {
    let (w, h) = (96u32, 48u32);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = ((x as f64 - 48.0) / 48.0, (y as f64 - 24.0) / 24.0);
            let r = (dx * dx + dy * dy).sqrt();
            let on = (0.6..=1.0).contains(&r);
            rgba.extend_from_slice(&[200, 20, 20, if on { 255 } else { 0 }]);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    out
}

fn has_chinese_font() -> bool {
    ["msyh.ttc", "simsun.ttc", "simhei.ttf"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file())
}

fn check_saved(original: &Path, saved: &Path, password: &str) {
    let before = std::fs::read(original).unwrap();
    let after = std::fs::read(saved).unwrap();
    assert!(after.len() > before.len(), "nothing was appended");
    assert!(after.starts_with(&before), "{}: the bytes of the original changed", saved.display());
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = if password.is_empty() { common::qpdf_check(&qpdf, saved) } else { common::qpdf_check_pw(&qpdf, saved, password) };
        let original_verdict = if password.is_empty() { common::qpdf_check(&qpdf, original) } else { common::qpdf_check_pw(&qpdf, original, password) };
        let rank = |code: i32| match code {
            0 => 0,
            3 => 1,
            _ => 2,
        };
        assert!(rank(verdict.code) <= rank(original_verdict.code), "{}: qpdf --check says {}:\n{}", saved.display(), verdict.code, verdict.text);
        let known = common::qpdf_complaints(&original_verdict.text, original);
        let new: Vec<String> = common::qpdf_complaints(&verdict.text, saved).into_iter().filter(|c| !known.contains(c)).collect();
        assert!(new.is_empty(), "{}: qpdf warns of what the original did not: {new:?}\n{}", saved.display(), verdict.text);
    }
    Document::open_with_password(saved, password).unwrap();
}

fn count_ink(b: &Rendered, test: impl Fn([u8; 3]) -> bool) -> usize {
    b.bgra.as_chunks::<4>().0.iter().filter(|p| test([p[2], p[1], p[0]])).count()
}

#[test]
fn a_chinese_form_is_filled_stamped_and_saved() {
    if !has_chinese_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    let out = out_dir();
    let input = out.join("zh-form.pdf");
    std::fs::write(&input, chinese_form("", "")).unwrap();
    let mut v = Viewer::open(&input, "");
    assert!(v.opened.rights.fill_forms && v.opened.rights.annotate);
    let fields = v.fields(0);
    assert_eq!(fields.len(), 8, "{fields:?}");
    assert_eq!(fields.iter().map(|f| f.kind).collect::<Vec<_>>(), vec![FieldKind::Text, FieldKind::Text, FieldKind::Text, FieldKind::Combo, FieldKind::Check, FieldKind::Radio, FieldKind::Radio, FieldKind::List]);
    assert_eq!(fields[2].max_len, 6);
    assert!(fields[2].comb && fields[1].multiline && !fields[3].editable);
    assert_eq!(v.options(0, &fields[3]).0.len(), 3);
    assert!(v.options(0, &fields[0]).0.is_empty(), "a text field has no options");
    // Every field is drawn before it is filled in (it has no appearance of its own): the frames show.
    let empty = v.render(0);
    let frame_pixels = count_ink(&empty, |c| c[0] < 90 && c[1] < 90 && c[2] < 90);
    assert!(frame_pixels > 600, "the frames of the fields are not drawn: {frame_pixels} dark pixels");

    let name = v.text(0, &fields[0], "张三（Zhang San）");
    assert_eq!(name.status, EditStatus::Done, "{}", name.message);
    let address = v.text(0, &fields[1], "北京市海淀区中关村大街1号\n邮编 100080 (Beijing) \\ 5号楼");
    assert_eq!(address.status, EditStatus::Done, "{}", address.message);
    assert_eq!(v.text(0, &fields[2], "1234567").status, EditStatus::Failed, "MaxLen is 6");
    assert_eq!(v.text(0, &fields[2], "100080").status, EditStatus::Done);
    assert_eq!(v.text(0, &fields[3], "上海").status, EditStatus::Done);
    assert_eq!(v.text(0, &fields[3], "天津").status, EditStatus::Failed, "not an option, and the box is not editable");
    assert_eq!(v.fill(0, &fields[4], FillValue::Checked(true)).status, EditStatus::Done);
    assert_eq!(v.fill(0, &fields[6], FillValue::Checked(true)).status, EditStatus::Done);
    assert_eq!(v.text(0, &fields[7], "香蕉").status, EditStatus::Done);
    let filled = v.render(0);
    let ink = count_ink(&filled, |c| c[2] > 120 && c[0] < 80);
    assert!(ink > 150, "the blue name is not drawn: {ink} pixels");

    let note = v.freetext(0, [150.0, 280.0, 290.0, 320.0], "审批意见：同意。\n2026年10月11日", 12.0, [200, 0, 0]);
    assert_eq!(note.status, EditStatus::Done, "{}", note.message);
    let stamp = v.stamp(0, [150.0, 320.0, 290.0, 395.0], signature_png());
    assert_eq!(stamp.status, EditStatus::Done, "{}", stamp.message);
    let stamped = v.render(0);
    let red = count_ink(&stamped, |c| c[0] > 150 && c[1] < 80 && c[2] < 80);
    assert!(red > 300, "the red text and the red ring are not drawn: {red} pixels");
    // The ring's middle is see-through: the page behind it (white, with a field's yellow in places) shows.
    let again = v.fields(0);
    assert_eq!(again[0].value, "张三（Zhang San）");
    assert_eq!(again[1].value, "北京市海淀区中关村大街1号\n邮编 100080 (Beijing) \\ 5号楼");
    assert_eq!(again[2].value, "100080");
    assert_eq!(again[3].value, "上海");
    assert!(again[4].checked && !again[5].checked && again[6].checked);
    assert_eq!(again[7].value, "香蕉");

    let saved = out.join("zh-form-filled.pdf");
    let (status, message, _) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{message}");
    check_saved(&input, &saved, "");

    // The saved file read again has all of it, by our reader's own eyes.
    let mut w = Viewer::open(&saved, "");
    let after = w.fields(0);
    assert_eq!(after.iter().map(|f| (f.value.clone(), f.checked)).collect::<Vec<_>>(), again.iter().map(|f| (f.value.clone(), f.checked)).collect::<Vec<_>>());
    let redrawn = w.render(0);
    assert_eq!(redrawn.bgra, stamped.bgra, "the saved file is drawn as the edited one was");
    // One undo takes the stamp away, and the text box under it is still there.
    let id = v.id();
    v.engine.undo(1, id);
    assert_eq!(v.edited(id).status, EditStatus::Done);
    let without = v.render(0);
    assert!(count_ink(&without, |c| c[0] > 150 && c[1] < 80 && c[2] < 80) < red);
}

#[test]
fn forms_that_are_not_to_be_filled_are_not() {
    let out = out_dir();
    // A certified file (DocMDP): P 2 allows filling in and nothing else, P 1 nothing at all.
    for (p, fill_ok, annotate_ok) in [(1, false, false), (2, true, false), (3, true, true)] {
        let mut objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] /DA (/Helv 12 Tf 0 g) >> /Perms << /DocMDP 20 0 R >> >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Annots [10 0 R] >>".to_string()),
            (10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /Rect [20 340 280 365] >>".to_string()),
        ];
        objects.push((20, format!("<< /Type /Sig /Filter /Adobe.PPKLite /Reference [ << /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P {p} /V /1.2 >> >> ] >>")));
        let path = out.join(format!("certified-{p}.pdf"));
        std::fs::write(&path, pdf(&objects, "")).unwrap();
        let mut v = Viewer::open(&path, "");
        let fields = v.fields(0);
        assert_eq!(fields.len(), 1);
        let r = v.text(0, &fields[0], "hello");
        assert_eq!(r.status == EditStatus::Done, fill_ok, "P {p}: filling: {r:?}");
        let r = v.freetext(0, [20.0, 20.0, 120.0, 60.0], "text", 12.0, [0, 0, 0]);
        assert_eq!(r.status == EditStatus::Done, annotate_ok, "P {p}: text box: {r:?}");
        if !annotate_ok {
            assert!(r.message.contains("DocMDP"), "{}", r.message);
        }
    }

    // A file whose author allows annotations but not filling in forms, and the other way round (made with qpdf, empty user password).
    let Some(qpdf) = common::find_qpdf() else { return };
    let input = out.join("zh-form-for-rights.pdf");
    std::fs::write(&input, chinese_form("", "")).unwrap();
    for (flags, annotate_ok, fill_ok) in [(["--annotate=n", "--form=y"], false, true), (["--annotate=y", "--form=n"], true, false)] {
        let locked = out.join(format!("rights-{}-{}.pdf", flags[0].trim_start_matches("--"), flags[1].trim_start_matches("--")));
        let status = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner", "256"]).args(flags).args(["--", &input.to_string_lossy(), &locked.to_string_lossy()]).status().unwrap();
        assert!(status.code().is_some_and(|c| c == 0 || c == 3));
        let mut v = Viewer::open(&locked, "");
        assert_eq!((v.opened.rights.annotate, v.opened.rights.fill_forms), (annotate_ok, fill_ok), "{flags:?}");
        let fields = v.fields(0);
        let r = v.text(0, &fields[0], "x");
        assert_eq!(r.status, if fill_ok { EditStatus::Done } else { EditStatus::Denied }, "{flags:?}: filling: {r:?}");
        let r = v.freetext(0, [20.0, 20.0, 120.0, 60.0], "text", 12.0, [0, 0, 0]);
        assert_eq!(r.status, if annotate_ok { EditStatus::Done } else { EditStatus::Denied }, "{flags:?}: text box: {r:?}");
        let r = v.stamp(0, [20.0, 20.0, 120.0, 60.0], signature_png());
        assert_eq!(r.status, if annotate_ok { EditStatus::Done } else { EditStatus::Denied }, "{flags:?}: stamp: {r:?}");
        // The owner password lifts it.
        let mut owner = Viewer::open(&locked, "owner");
        assert!(owner.opened.rights.annotate && owner.opened.rights.fill_forms);
        let fields = owner.fields(0);
        assert_eq!(owner.text(0, &fields[0], "x").status, EditStatus::Done);
        let saved = out.join("rights-saved.pdf");
        assert_eq!(owner.save(&saved).0, SaveStatus::Done);
        check_saved(&locked, &saved, "owner");
    }
}

#[test]
fn hostile_pictures_and_boxes_are_refused_without_harm() {
    let out = out_dir();
    let input = out.join("zh-form-hostile.pdf");
    std::fs::write(&input, chinese_form("", "")).unwrap();
    let mut v = Viewer::open(&input, "");
    let fine = [20.0, 20.0, 120.0, 60.0];
    // Not a picture, a cut one, one that claims to be enormous, and a PNG of 17 million pixels (the header only is needed to refuse).
    assert_eq!(v.stamp(0, fine, b"not a picture".to_vec()).status, EditStatus::Failed);
    let png = signature_png();
    assert_eq!(v.stamp(0, fine, png[..40].to_vec()).status, EditStatus::Failed);
    let mut big = png.clone();
    big[16..20].copy_from_slice(&5000u32.to_be_bytes());
    big[20..24].copy_from_slice(&5000u32.to_be_bytes());
    let r = v.stamp(0, fine, big);
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    assert_eq!(v.stamp(0, fine, vec![0xFF; 13 << 20]).status, EditStatus::Failed);
    // Boxes: no text, text that is not a number's worth of room, places that are not numbers.
    assert_eq!(v.freetext(0, fine, "  \n ", 12.0, [0, 0, 0]).status, EditStatus::Failed);
    assert_eq!(v.freetext(0, [f32::NAN, 0.0, 10.0, 10.0], "x", 12.0, [0, 0, 0]).status, EditStatus::Failed);
    assert_eq!(v.freetext(0, fine, "x", f32::NAN, [0, 0, 0]).status, EditStatus::Failed);
    // A size that is too big or too small is held to the limits; a place off the page is moved onto it.
    assert_eq!(v.freetext(0, [-500.0, -500.0, 5000.0, 5000.0], "big", 1000.0, [0, 0, 0]).status, EditStatus::Done);
    assert_eq!(v.freetext(0, [290.0, 390.0, 295.0, 399.0], "tiny", 0.01, [0, 0, 0]).status, EditStatus::Done);
    // A character beyond the Basic Multilingual Plane (some font has it, or none does: either way nothing breaks), and one that no
    // font has (a private use character): the field is filled in, or it says why not.
    let fields = v.fields(0);
    assert!(matches!(v.text(0, &fields[0], "emoji \u{1F600}").status, EditStatus::Done | EditStatus::Failed));
    let r = v.text(0, &fields[0], "nothing has \u{10FFFD}");
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    assert!(r.message.contains("no installed font has U+10FFFD"), "{}", r.message);
    // Plain ASCII, a lot of text and a lot of different characters.
    assert_eq!(v.freetext(0, fine, &"The quick brown fox. ".repeat(190), 8.0, [0, 0, 0]).status, EditStatus::Done);
    let many: String = (0x4E00u32..0x4E00 + 3500).filter_map(char::from_u32).collect();
    let r = v.freetext(0, fine, &many, 8.0, [0, 0, 0]);
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    // The file is still whole: nothing but the two boxes and the text was added.
    let saved = out.join("zh-form-hostile-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&input, &saved, "");
}

fn many_fields(count: u32) -> Vec<u8> {
    let mut fields = String::new();
    let mut annots = String::new();
    let mut objects = Vec::new();
    for i in 0..count {
        let n = 100 + i;
        fields.push_str(&format!("{n} 0 R "));
        annots.push_str(&format!("{n} 0 R "));
        let (col, row) = (i % 5, i / 5);
        let (x, y) = (20.0 + col as f64 * 110.0, 760.0 - row as f64 * 7.5);
        objects.push((n, format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T (f{i}) /V (value {i}) /Rect [{x} {y} {} {}] /MK << /BC [0 0 0] >> /DA (/Helv 6 Tf 0 g) >>", x + 100.0, y + 7.0)));
    }
    let mut all = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{fields}] /DA (/Helv 0 Tf 0 g) /NeedAppearances true >> >>")),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [{annots}] >>")),
    ];
    all.extend(objects);
    pdf(&all, "")
}

#[test]
fn a_form_of_500_fields_opens_and_saves_quickly() {
    let out = out_dir();
    let path = out.join("form-500.pdf");
    std::fs::write(&path, many_fields(500)).unwrap();
    let started = Instant::now();
    let mut v = Viewer::open(&path, "");
    let fields = v.fields(0);
    let drawn = v.render(0);
    let open_time = started.elapsed();
    assert_eq!(fields.len(), 500);
    assert!(fields.iter().all(|f| f.kind == FieldKind::Text));
    assert!(count_ink(&drawn, |c| c[0] < 100) > 2000, "the values are not drawn");
    let started = Instant::now();
    let r = v.text(0, &fields[250], "changed");
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
    let saved = out.join("form-500-saved.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    let fill_time = started.elapsed();
    println!("500 fields: open + fields + first drawing {open_time:?}, fill one + save {fill_time:?}");
    if !cfg!(debug_assertions) && std::env::var_os("QINGPDF_NO_TIMING").is_none() {
        assert!(open_time < Duration::from_millis(1000), "opening took {open_time:?}");
        assert!(fill_time < Duration::from_millis(200), "filling one field and saving took {fill_time:?}");
    }
    check_saved(&path, &saved, "");
    let doc = Document::open(&saved).unwrap();
    assert!(doc.page_count().unwrap() == 1 && matches!(doc.get(qingpdf_core::object::ObjRef::new(350, 0)).unwrap(), Object::Dict(_)));
}

#[test]
fn pages_that_are_turned_show_filled_fields_text_boxes_and_stamps_upright() {
    if !has_chinese_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    let out = out_dir();
    for rotate in [0, 90, 180, 270] {
        let input = out.join(format!("rot-{rotate}.pdf"));
        std::fs::write(&input, chinese_form("", &format!("/Rotate {rotate}"))).unwrap();
        let mut v = Viewer::open(&input, "");
        let fields = v.fields(0);
        assert_eq!(fields.len(), 8);
        // The fields are where the page as shown has them: a turned page swaps width and height.
        let (pw, ph) = (v.opened.pages[0].width, v.opened.pages[0].height);
        assert_eq!((pw, ph), if rotate % 180 == 90 { (400.0, 300.0) } else { (300.0, 400.0) });
        assert!(fields.iter().all(|f| f.rect[0] >= 0.0 && f.rect[2] <= pw as f32 + 0.5 && f.rect[1] >= 0.0 && f.rect[3] <= ph as f32 + 0.5), "{rotate}: {fields:?}");
        assert_eq!(v.text(0, &fields[0], "张三").status, EditStatus::Done);
        assert_eq!(v.fill(0, &fields[4], FillValue::Checked(true)).status, EditStatus::Done);
        assert_eq!(v.text(0, &fields[3], "广州").status, EditStatus::Done);
        let r = v.freetext(0, [20.0, 20.0, 150.0, 60.0], "审批意见：同意", 12.0, [200, 0, 0]);
        assert_eq!(r.status, EditStatus::Done, "{}", r.message);
        let r = v.stamp(0, [pw as f32 - 120.0, ph as f32 - 80.0, pw as f32 - 10.0, ph as f32 - 10.0], signature_png());
        assert_eq!(r.status, EditStatus::Done, "{}", r.message);
        let saved = out.join(format!("rot-{rotate}-filled.pdf"));
        assert_eq!(v.save(&saved).0, SaveStatus::Done);
        check_saved(&input, &saved, "");
        let mut w = Viewer::open(&saved, "");
        let again = w.fields(0);
        assert_eq!((again[0].value.as_str(), again[3].value.as_str(), again[4].checked), ("张三", "广州", true));
    }
}

/// Fill in every field of the forms of the corpus that can be (text, choices, boxes) and save: the file stays whole.
fn fill_all(name: &str, input: &Path, password: &str) {
    let out = out_dir();
    let mut v = Viewer::open(input, password);
    let fields = v.fields(0);
    assert!(!fields.is_empty(), "{name}: no fields");
    let mut done = 0;
    for f in &fields {
        let r = match f.kind {
            FieldKind::Text => v.text(0, f, "测试文字 Test (1)"),
            FieldKind::Combo if f.editable => v.text(0, f, "测试文字 Test (1)"),
            FieldKind::Combo | FieldKind::List => match v.options(0, f).0.first() {
                Some((export, _)) => v.fill(0, f, FillValue::Text(export.clone())),
                None => continue,
            },
            FieldKind::Check => v.fill(0, f, FillValue::Checked(!f.checked)),
            FieldKind::Radio => v.fill(0, f, FillValue::Checked(true)),
            FieldKind::Push | FieldKind::Signature => continue,
        };
        assert!(matches!(r.status, EditStatus::Done | EditStatus::Failed), "{name}: {:?} {r:?}", f.kind);
        if r.status == EditStatus::Done {
            done += 1;
        }
    }
    assert!(done >= 1, "{name}: nothing could be filled in");
    let saved = out.join(format!("{name}-filled.pdf"));
    let (status, message, _) = v.save(&saved);
    assert_eq!(status, SaveStatus::Done, "{name}: {message}");
    check_saved(input, &saved, password);
    let mut w = Viewer::open(&saved, password);
    let again = w.fields(0);
    assert_eq!(again.len(), fields.len(), "{name}");
    println!("{name}: {done} of {} fields filled in", fields.len());
}

#[test]
fn the_forms_of_the_corpus_are_filled_in_and_saved() {
    if !has_chinese_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    fill_all("multiple_form_types", &common::public_file("outline-form-attach/multiple_form_types.pdf"), "");
    // An R6 file whose permissions allow annotations but not forms: no field is filled in, a text box can be put.
    let locked = common::public_file("encrypted/qpdf-generated/forms.r6-aes256-empty-forms.pdf");
    let mut v = Viewer::open(&locked, "");
    assert!(v.opened.rights.annotate && !v.opened.rights.fill_forms);
    let fields = v.fields(0);
    assert_eq!(fields.len(), 5);
    let text = fields.iter().find(|f| f.kind == FieldKind::Text).unwrap();
    assert_eq!(v.text(0, text, "x").status, EditStatus::Denied);
    assert_eq!(v.freetext(0, [100.0, 400.0, 300.0, 440.0], "表单不许填，文字框可以。", 14.0, [0, 0, 0]).status, EditStatus::Done);
    let saved = out_dir().join("forms-r6-textbox.pdf");
    assert_eq!(v.save(&saved).0, SaveStatus::Done);
    check_saved(&locked, &saved, "");
    // The files of the local corpus (not in the repository) when they are there: forms with appearances, in files with
    // cross-reference streams and hybrid ones.
    for rel in ["xref-stream-objstm/issue17492.pdf", "hybrid/issue12706.pdf", "xref-stream-objstm/bug1811510.pdf"] {
        let path = common::corpus_root().join("local").join(rel);
        if path.is_file() {
            fill_all(&Path::new(rel).file_stem().unwrap().to_string_lossy(), &path, "");
        }
    }
}

// --- hostile forms (the review of 4b) --------------------------------------------------------------------------------------
//
// Each of these took minutes, or hundreds of megabytes, before: what a widget inherits (a long `/Opt`, a huge default `/DA`) was
// read and copied again for every widget, and every list of fields carried every option. The memory they would take is checked by
// what the engine sends (the lists are bounded in bytes); the process's own peak was measured with a harness outside the tests
// (no `unsafe` allocator hooks are allowed in this workspace outside `win32.rs`).

use qingpdf_core::view::{MAX_FIELDS_BYTES, MAX_OPTION_BYTES, RenderStatus};

/// A time limit that is asserted in release builds only (the same switch as the speed check of 500 fields).
fn within<T>(what: &str, limit: Duration, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let done = work();
    let took = started.elapsed();
    println!("{what}: {took:?}");
    if !cfg!(debug_assertions) && std::env::var_os("QINGPDF_NO_TIMING").is_none() {
        assert!(took < limit, "{what} took {took:?}");
    }
    done
}

/// A page of `count` widgets, 12 by 12 points, that have no appearance. With a `parent` (object 5, which says where its `/Kids` go
/// with `@KIDS@`) they are all its kids; otherwise each is a text field of its own with `widget` as extra entries. `extra`: more
/// objects.
fn crowd(count: u32, acro_da: &str, parent: Option<String>, widget: &str, extra: Vec<(u32, String)>) -> Vec<u8> {
    let kids: Vec<u32> = (0..count).map(|i| 100 + i).collect();
    let refs = |ns: &[u32]| ns.iter().map(|n| format!("{n} 0 R")).collect::<Vec<_>>().join(" ");
    let top = if parent.is_some() { "5 0 R".to_string() } else { refs(&kids) };
    let mut all = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{top}] /DA ({acro_da}) /DR << /Font << /Helv 4 0 R >> >> >> >>")),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Annots [{}] >>", refs(&kids))),
        (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()),
    ];
    if let Some(p) = &parent {
        all.push((5, p.replace("@KIDS@", &refs(&kids))));
    }
    for (i, n) in kids.iter().enumerate() {
        let (x, y) = (10 + (i % 40) * 14, 10 + (i / 40 % 50) * 14);
        let rect = format!("[{x} {y} {} {}]", x + 12, y + 12);
        all.push((
            *n,
            if parent.is_some() {
                format!("<< /Type /Annot /Subtype /Widget /Parent 5 0 R /Rect {rect} /F 4 {widget} >>")
            } else {
                format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T (f{i}) /V (x) /Rect {rect} /F 4 {widget} >>")
            },
        ));
    }
    all.extend(extra);
    pdf(&all, "")
}

/// The bytes a list of fields takes in the engine's message.
fn field_bytes(fields: &[FieldInfo]) -> usize {
    fields.iter().map(|f| std::mem::size_of::<FieldInfo>() + f.value.len() + f.name.len()).sum()
}

fn option_bytes(items: &[(String, String)]) -> usize {
    items.iter().map(|o| o.0.len() + o.1.len()).sum()
}

fn open_crowd(name: &str, bytes: Vec<u8>) -> Viewer {
    let path = out_dir().join(name);
    std::fs::write(&path, bytes).unwrap();
    Viewer::open(&path, "")
}

#[test]
fn forty_combo_boxes_that_share_one_long_option_list_send_no_option_lists_and_draw_quickly() {
    // 2000 options, all the same 4000 characters, shared by 40 combo boxes through their parent: 16 MB of text each, 610 MB for all.
    let parent = "<< /FT /Ch /Ff 131072 /T (c) /V (A) /Opt 6 0 R /Kids [@KIDS@] >>".to_string();
    let extra = vec![(6, format!("[{}]", "7 0 R ".repeat(2000))), (7, format!("({})", "A".repeat(4000)))];
    let mut v = open_crowd("limits-shared-options.pdf", crowd(40, "/Helv 0 Tf 0 g", Some(parent), "", extra));
    let fields = within("40 combo boxes: fields", Duration::from_secs(2), || v.fields(0));
    assert_eq!(fields.len(), 40);
    assert!(fields.iter().all(|f| f.kind == FieldKind::Combo));
    assert!(field_bytes(&fields) <= MAX_FIELDS_BYTES, "{} bytes", field_bytes(&fields));
    // The options come when one field is chosen from, cut to a size.
    let (items, truncated) = within("40 combo boxes: options of each", Duration::from_secs(2), || {
        let mut last = (Vec::new(), false);
        for f in &fields {
            last = v.options(0, f);
        }
        last
    });
    assert!(truncated && !items.is_empty() && option_bytes(&items) <= MAX_OPTION_BYTES, "{} items, {} bytes", items.len(), option_bytes(&items));
    let drawn = within("40 combo boxes: drawing", Duration::from_secs(2), || v.render(0));
    assert_eq!(drawn.status, RenderStatus::Complete, "{}", drawn.message);
    // Filling one in is as quick.
    let r = within("40 combo boxes: fill one", Duration::from_secs(2), || v.text(0, &fields[3], &items[0].0));
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
}

#[test]
fn a_form_whose_default_appearance_string_is_eight_megabytes_is_read_once() {
    let da = format!("{}/Helv 0 Tf 0 g", "1 ".repeat(4_000_000));
    let mut v = open_crowd("limits-huge-da.pdf", crowd(3000, &da, None, "", Vec::new()));
    let fields = within("3000 fields, 8 MB default DA: fields", Duration::from_secs(2), || v.fields(0));
    assert!(fields.len() > 1000 && field_bytes(&fields) <= MAX_FIELDS_BYTES + 2 * std::mem::size_of::<FieldInfo>() + 40_000);
    let drawn = within("3000 fields, 8 MB default DA: drawing", Duration::from_secs(2), || v.render(0));
    assert_eq!(drawn.status, RenderStatus::Complete, "{}", drawn.message);
    assert!(count_ink(&drawn, |c| c[0] < 100) > 1000);
    let r = within("3000 fields, 8 MB default DA: fill one and save", Duration::from_secs(2), || {
        let r = v.text(0, &fields[5], "changed");
        let saved = out_dir().join("limits-huge-da-saved.pdf");
        assert_eq!(v.save(&saved).0, SaveStatus::Done);
        r
    });
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
}

#[test]
fn three_thousand_combo_boxes_that_share_three_hundred_thousand_options_are_cut_and_quick() {
    let parent = "<< /FT /Ch /Ff 131072 /T (c) /V (A) /Opt 6 0 R /Kids [@KIDS@] >>".to_string();
    let extra = vec![(6, format!("[{}]", "7 0 R ".repeat(300_000))), (7, "(opt)".to_string())];
    let mut v = open_crowd("limits-many-options.pdf", crowd(3000, "/Helv 0 Tf 0 g", Some(parent), "", extra));
    let fields = within("3000 combo boxes: fields", Duration::from_secs(2), || v.fields(0));
    assert!(fields.len() > 1000 && fields.iter().all(|f| f.kind == FieldKind::Combo));
    let (items, truncated) = within("3000 combo boxes: options of one", Duration::from_secs(2), || v.options(0, &fields[7]));
    assert!(truncated && items.len() == 2000 && option_bytes(&items) <= MAX_OPTION_BYTES);
    let drawn = within("3000 combo boxes: drawing", Duration::from_secs(2), || v.render(0));
    assert_eq!(drawn.status, RenderStatus::Complete, "{}", drawn.message);
    let r = within("3000 combo boxes: fill one", Duration::from_secs(2), || v.text(0, &fields[7], "opt"));
    assert_eq!(r.status, EditStatus::Done, "{}", r.message);
}

#[test]
fn fields_without_appearances_are_drawn_on_every_drawing_and_a_cap_on_a_page_is_told() {
    // 1500 fields, drawn again and again (the cap counts for one drawing of a page, not for the life of the document).
    let mut v = open_crowd("limits-1500.pdf", crowd(1500, "/Helv 0 Tf 0 g", None, "", Vec::new()));
    let first = v.render(0);
    let ink = count_ink(&first, |c| c[0] < 100);
    assert!(first.status == RenderStatus::Complete && ink > 1000, "{:?} {ink}", first.status);
    for n in 2..=30 {
        let again = within("1500 fields: one more drawing", Duration::from_secs(2), || v.render(0));
        assert_eq!(again.status, RenderStatus::Complete, "drawing {n}: {}", again.message);
        assert_eq!(count_ink(&again, |c| c[0] < 100), ink, "drawing {n} lost fields");
    }
    // 10,001 on one page: the last is not drawn, and the drawing says so (and says it again the next time).
    let mut v = open_crowd("limits-10001.pdf", crowd(10_001, "/Helv 0 Tf 0 g", None, "", Vec::new()));
    for _ in 0..2 {
        let drawn = within("10,001 fields: drawing", Duration::from_secs(5), || v.render(0));
        assert_eq!(drawn.status, RenderStatus::Incomplete, "{}", drawn.message);
        assert!(drawn.message.contains("10000 form fields"), "{}", drawn.message);
        assert!(count_ink(&drawn, |c| c[0] < 100) > 1000);
    }
    // 20,001 annotations: the same for the cap on all the annotations of a page.
    let mut v = open_crowd("limits-20001.pdf", crowd(20_001, "/Helv 0 Tf 0 g", None, "", Vec::new()));
    let drawn = within("20,001 fields: drawing", Duration::from_secs(5), || v.render(0));
    assert_eq!(drawn.status, RenderStatus::Incomplete, "{}", drawn.message);
    assert!(drawn.message.contains("form fields") || drawn.message.contains("annotations"), "{}", drawn.message);
}

#[test]
fn a_text_box_or_field_of_more_than_two_thousand_lines_is_refused_not_cut() {
    if !has_chinese_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let mut v = open_crowd("limits-lines.pdf", crowd(1, "/Helv 0 Tf 0 g", None, "/Ff 4096", Vec::new()));
    let r = v.freetext(0, [20.0, 20.0, 200.0, 60.0], &format!("{}x", "\n".repeat(2100)), 8.0, [0, 0, 0]);
    assert_eq!(r.status, EditStatus::Failed, "{r:?}");
    assert!(r.message.contains("2000 lines"), "{}", r.message);
    // A multi-line field of a few lines' height is filled in: the lines it could not show are not laid out and are not missed
    // (the layout is tested with a tall box in `layout.rs`).
    let fields = v.fields(0);
    let r = v.text(0, &fields[0], &format!("{}x", "\n".repeat(2100)));
    assert_eq!(r.status, EditStatus::Done, "{r:?}");
}
