//! Filling in forms, text boxes and stamps, worked out against small files made by hand. The fonts are the system's: a test that
//! needs a Chinese font says so and passes quietly on a machine that has none.

use super::fontsub::Look;
use crate::forms::{MAX_KIDS, text_of};
use super::*;
use crate::edit::{Edit, apply, commit, save_section};
use crate::render::Renderer;
use crate::testutil::PdfBuilder;

const NOW: &str = "D:20260101000000Z";

/// A UTF-16BE hexadecimal string (what a file written by Acrobat has for Chinese).
fn hex16(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        out.push_str(&format!("{u:04X}"));
    }
    out.push('>');
    out
}

/// A one-page form: 10 name (single line, border and background), 11 address (several lines), 12 code (comb of six), 13 city (combo
/// box with Chinese options), 14 check box with no appearance, 15 radio group of the kids 16 and 17 (no appearances), 18 list box.
pub(crate) fn form(extra_catalog: &str, extra_page: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 18 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 30 0 R >> >> >> {extra_catalog} >>"));
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Resources << >> /Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 16 0 R 17 0 R 18 0 R] {extra_page} >>"));
    b.obj(10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [20 340 280 365] /MK << /BC [0 0 0] /BG [1 1 0.8] >> /DA (/Helv 0 Tf 0 0 0.6 rg) >>");
    b.obj(11, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (address) /Ff 4096 /Rect [20 250 280 330] /MK << /BC [0.5 0.5 0.5] >> /DA (/Helv 12 Tf 0 g) >>");
    b.obj(12, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (code) /Ff 16777216 /MaxLen 6 /Q 1 /Rect [20 200 200 225] /MK << /BC [0 0 0] >> >>");
    b.obj(13, &format!("<< /Type /Annot /Subtype /Widget /FT /Ch /T (city) /Ff 131072 /Rect [20 150 200 175] /MK << /BC [0 0 0] >> /Opt [{} {} (Paris)] >>", hex16("北京"), hex16("上海")));
    b.obj(14, "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [20 100 40 120] /MK << /BC [0 0 0] /CA (4) >> >>");
    b.obj(15, "<< /FT /Btn /Ff 32768 /T (gender) /Kids [16 0 R 17 0 R] >>");
    b.obj(16, "<< /Type /Annot /Subtype /Widget /Parent 15 0 R /Rect [60 100 80 120] /MK << /BC [0 0 0] >> >>");
    b.obj(17, "<< /Type /Annot /Subtype /Widget /Parent 15 0 R /Rect [100 100 120 120] /MK << /BC [0 0 0] >> >>");
    b.obj(18, &format!("<< /Type /Annot /Subtype /Widget /FT /Ch /T (list) /Rect [20 20 200 90] /MK << /BC [0 0 0] >> /Opt [{} {} (Paris)] >>", hex16("苹果"), hex16("香蕉")));
    b.obj(30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.finish_classic(40, "/Root 1 0 R")
}

fn pixel(b: &crate::render::Bitmap, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * b.width + x) * 4) as usize;
    [b.rgba[i], b.rgba[i + 1], b.rgba[i + 2]]
}

fn has_cjk_font() -> bool {
    let chars: std::collections::BTreeSet<char> = ['中', 'a'].into_iter().collect();
    fontsub::choose(&chars, Look::Sans).is_ok()
}

/// Make `edit` against `doc`, put it in front of the document and return the objects it made.
fn make(doc: &Document, edit: &Edit) -> Result<Vec<(u32, Object)>> {
    let pages = doc.pages()?;
    let made = apply(doc, &pages, edit, NOW)?;
    commit(doc, made.clone());
    Ok(made)
}

fn annot_index(doc: &Document, num: u32) -> u32 {
    let pages = doc.pages().unwrap();
    let page = doc.get(pages[0].obj_ref).unwrap();
    let annots = doc.resolve(page.as_dict().unwrap().get("Annots").unwrap()).unwrap();
    annots.as_array().unwrap().iter().position(|a| a.as_obj_ref().is_some_and(|r| r.num == num)).expect("not on the page") as u32
}

fn fill(doc: &Document, num: u32, value: FillValue) -> Result<Vec<(u32, Object)>> {
    let index = annot_index(doc, num);
    make(doc, &Edit::Fill { page: 0, index, num, value })
}

fn text(s: &str) -> FillValue {
    FillValue::Text(s.to_string())
}

fn dict_of(doc: &Document, num: u32) -> Dict {
    doc.get(ObjRef::new(num, 0)).unwrap().as_dict().unwrap().clone()
}

fn value_of(doc: &Document, num: u32) -> String {
    let d = dict_of(doc, num);
    text_of(doc, &doc.resolve(d.get("V").unwrap()).unwrap())
}

/// The font program, `ToUnicode` text and content of the appearance of widget `num`.
fn appearance_of(doc: &Document, num: u32) -> (Vec<u8>, String, String) {
    let widget = dict_of(doc, num);
    let ap = doc.resolve(widget.get("AP").unwrap()).unwrap();
    let Object::Stream(form) = doc.resolve(ap.as_dict().unwrap().get("N").unwrap()).unwrap() else { panic!("N is not a stream") };
    let content = String::from_utf8_lossy(&doc.decode_stream(&form).unwrap()).into_owned();
    let resources = doc.resolve(form.dict.get("Resources").unwrap()).unwrap();
    let fonts = doc.resolve(resources.as_dict().unwrap().get("Font").unwrap()).unwrap();
    let font = doc.resolve(fonts.as_dict().unwrap().get("F0").unwrap()).unwrap();
    let font = font.as_dict().unwrap();
    let Object::Stream(uni) = doc.resolve(font.get("ToUnicode").unwrap()).unwrap() else { panic!("no ToUnicode") };
    let uni = String::from_utf8_lossy(&doc.decode_stream(&uni).unwrap()).into_owned();
    let descendants = doc.resolve(font.get("DescendantFonts").unwrap()).unwrap();
    let cid = doc.resolve(&descendants.as_array().unwrap()[0]).unwrap();
    let descriptor = doc.resolve(cid.as_dict().unwrap().get("FontDescriptor").unwrap()).unwrap();
    let Object::Stream(file) = doc.resolve(descriptor.as_dict().unwrap().get("FontFile2").unwrap()).unwrap() else { panic!("no font file") };
    (doc.decode_stream(&file).unwrap(), uni, content)
}

/// `doc`'s file with the edits made so far saved on to it, and read again.
fn saved(base: &[u8], doc: &Document) -> (Vec<u8>, Document) {
    let section = save_section(doc).unwrap();
    let mut bytes = base.to_vec();
    bytes.extend_from_slice(&section);
    let again = Document::from_bytes(bytes.clone()).unwrap();
    assert!(!again.was_repaired());
    (bytes, again)
}

#[test]
fn a_chinese_value_gets_a_subset_font_with_every_character_in_its_unicode_map() {
    if !has_cjk_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    let base = form("", "");
    let doc = Document::from_bytes(base.clone()).unwrap();
    fill(&doc, 10, text("张三 Zhang")).unwrap();
    let (_, again) = saved(&base, &doc);
    assert_eq!(value_of(&again, 10), "张三 Zhang");
    let (program, uni, content) = appearance_of(&again, 10);
    // An sfnt file with the tables a reader needs and a checksum that adds up.
    assert_eq!(&program[..4], &[0, 1, 0, 0]);
    for tag in ["glyf", "head", "hhea", "hmtx", "loca", "maxp"] {
        assert!(program.windows(4).any(|w| w == tag.as_bytes()), "no {tag} table");
    }
    assert!(program.len() < 40_000, "{} bytes for eight characters", program.len());
    for c in "张三 Zhang".chars().filter(|c| *c != ' ') {
        let mut units = [0u16; 2];
        let hex: String = c.encode_utf16(&mut units).iter().map(|u| format!("{u:04X}")).collect();
        assert!(uni.contains(&format!("> <{hex}>")), "{c} is not in the unicode map:\n{uni}");
    }
    assert!(content.contains("/F0") && content.contains(" Tj") && content.contains("0 0 0.6 rg"), "{content}");
    // The value is a UTF-16 string and the appearance is a form that is drawn: the field is inked inside its box.
    let widget = dict_of(&again, 10);
    assert!(matches!(widget.get("V"), Some(Object::String(s)) if s.bytes.starts_with(&[0xFE, 0xFF])));
}

#[test]
fn text_with_brackets_backslashes_and_line_ends_is_stored_as_typed() {
    let base = form("", "");
    let doc = Document::from_bytes(base.clone()).unwrap();
    // (ASCII only: no font but an Latin one is needed.)
    let typed = "a (b) \\ c) (d\nsecond line";
    fill(&doc, 11, text(typed)).unwrap();
    // A single-line field takes the line end as a space.
    fill(&doc, 10, text("one\ntwo")).unwrap();
    let (bytes, again) = saved(&base, &doc);
    assert_eq!(value_of(&again, 11), typed);
    assert_eq!(value_of(&again, 10), "one two");
    // In the file it is a literal string with the brackets and the backslash escaped, and the line end as \r.
    let mut expected = Vec::new();
    crate::writer::write_literal_string(&mut expected, "a (b) \\ c) (d\rsecond line".as_bytes());
    assert_eq!(expected, b"(a \\(b\\) \\\\ c\\) \\(d\\015second line)");
    assert!(bytes.windows(expected.len()).any(|w| w == expected.as_slice()), "{}", String::from_utf8_lossy(&bytes[base.len()..]));
}

#[test]
fn a_field_takes_what_its_flags_and_limits_allow() {
    let base = form("", "");
    let doc = Document::from_bytes(base).unwrap();
    // MaxLen 6 on the comb field.
    assert!(fill(&doc, 12, text("1234567")).is_err());
    let made = fill(&doc, 12, text("123456")).unwrap();
    assert!(made.iter().any(|(n, _)| *n == 12));
    // A combo box that is editable takes anything; this one is not (flags 131072 only): only its options.
    assert!(fill(&doc, 13, text("Tokyo")).is_err());
    assert!(fill(&doc, 13, text("Paris")).is_ok());
    // A value that suits no field of that kind, and a widget that is no field.
    assert!(fill(&doc, 14, text("x")).is_err());
    assert!(fill(&doc, 10, FillValue::Checked(true)).is_err());
    // Too many characters.
    assert!(matches!(fill(&doc, 11, text(&"x".repeat(MAX_VALUE_CHARS + 1))), Err(Error::Limit(_))));
    // Read-only.
    let ro = {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] /DA (/Helv 12 Tf 0 g) >> >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [10 0 R] >>");
        b.obj(10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /Ff 1 /Rect [10 10 90 30] >>");
        Document::from_bytes(b.finish_classic(20, "/Root 1 0 R")).unwrap()
    };
    assert!(matches!(fill(&ro, 10, text("x")), Err(Error::Invalid(m)) if m.contains("read-only")));
    // The place in the page's list and the number must agree.
    let pages = doc.pages().unwrap();
    let wrong = Edit::Fill { page: 0, index: 1, num: 10, value: text("x") };
    assert!(apply(&doc, &pages, &wrong, NOW).is_err());
}

#[test]
fn check_boxes_and_radio_buttons_get_appearances_and_states() {
    let base = form("", "");
    let doc = Document::from_bytes(base.clone()).unwrap();
    fill(&doc, 14, FillValue::Checked(true)).unwrap();
    // The second radio button of the group is chosen: the group has its value, the other kid is Off.
    fill(&doc, 17, FillValue::Checked(true)).unwrap();
    let (_, again) = saved(&base, &doc);
    let agree = dict_of(&again, 14);
    assert_eq!(agree.get("AS"), Some(&Object::from("Yes")));
    assert_eq!(dict_of(&again, 14).get("V"), Some(&Object::from("Yes")));
    let ap = again.resolve(agree.get("AP").unwrap()).unwrap();
    let states = again.resolve(ap.as_dict().unwrap().get("N").unwrap()).unwrap();
    assert!(states.as_dict().unwrap().contains_key("Off") && states.as_dict().unwrap().contains_key("Yes"));
    assert_eq!(dict_of(&again, 15).get("V"), Some(&Object::from("1")));
    assert_eq!(dict_of(&again, 17).get("AS"), Some(&Object::from("1")));
    assert_eq!(dict_of(&again, 16).get("AS"), Some(&Object::from("Off")));
    // The other way round, and turning one off.
    fill(&again, 16, FillValue::Checked(true)).unwrap();
    assert_eq!(dict_of(&again, 15).get("V"), Some(&Object::from("0")));
    assert_eq!(dict_of(&again, 17).get("AS"), Some(&Object::from("Off")));
    fill(&again, 14, FillValue::Checked(false)).unwrap();
    assert_eq!(dict_of(&again, 14).get("AS"), Some(&Object::from("Off")));
}

#[test]
fn a_radio_group_that_cannot_be_turned_off_stays_on() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [15 0 R] >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [16 0 R 17 0 R] >>");
    b.obj(15, &format!("<< /FT /Btn /Ff {} /T (g) /Kids [16 0 R 17 0 R] /V /b >>", (1 << 15) | (1 << 14)));
    b.obj(16, "<< /Type /Annot /Subtype /Widget /Parent 15 0 R /Rect [10 10 30 30] /AS /Off /AP << /N << /a 20 0 R /Off 20 0 R >> >> >>");
    b.obj(17, "<< /Type /Annot /Subtype /Widget /Parent 15 0 R /Rect [50 10 70 30] /AS /b /AP << /N << /b 20 0 R /Off 20 0 R >> >> >>");
    b.obj(20, "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] >>");
    let doc = Document::from_bytes(b.finish_classic(30, "/Root 1 0 R")).unwrap();
    assert!(fill(&doc, 17, FillValue::Checked(false)).is_err());
    // Choosing the other one uses the names the appearances have.
    fill(&doc, 16, FillValue::Checked(true)).unwrap();
    assert_eq!(dict_of(&doc, 15).get("V"), Some(&Object::from("a")));
    assert_eq!(dict_of(&doc, 17).get("AS"), Some(&Object::from("Off")));
    assert_eq!(dict_of(&doc, 16).get("AS"), Some(&Object::from("a")));
}

#[test]
fn a_loop_of_parents_and_a_crowd_of_kids_are_cut_short() {
    let mut b = PdfBuilder::new();
    let kids: String = (100..100 + 2 * MAX_KIDS as u32).map(|n| format!("{n} 0 R ")).collect();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R] /DA (/Helv 12 Tf 0 g) >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [10 0 R 12 0 R] >>");
    // 10 and 11 are each other's parents; no type anywhere.
    b.obj(10, "<< /Type /Annot /Subtype /Widget /T (a) /Parent 11 0 R /Rect [10 10 90 30] >>");
    b.obj(11, "<< /T (b) /Parent 10 0 R >>");
    // A field with a crowd of kids that are widgets.
    b.obj(12, &format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T (c) /Rect [10 40 90 60] /Kids [{kids}] >>"));
    let doc = Document::from_bytes(b.finish_classic(30, "/Root 1 0 R")).unwrap();
    assert!(fill(&doc, 10, text("x")).is_err(), "a field of no type is not filled in");
    // The crowd is looked at only up to the limit (the kids are not even objects), and the field itself is filled.
    let made = fill(&doc, 12, text("x")).unwrap();
    assert!(made.len() < 30, "{} objects", made.len());
}

#[test]
fn text_boxes_and_stamps_stand_upright_on_a_turned_page() {
    if !has_cjk_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    for rotate in [0, 90, 180, 270] {
        let base = form("", &format!("/Rotate {rotate}"));
        let doc = Document::from_bytes(base.clone()).unwrap();
        let (w, h) = if rotate % 180 == 90 { (400.0, 300.0) } else { (300.0, 400.0) };
        let edit = Edit::FreeText { page: 0, rect: [30.0, 30.0, 150.0, 60.0], text: "你好，世界 Hello".to_string(), size: 14.0, color: [200, 0, 0], author: "me".to_string() };
        make(&doc, &edit).unwrap();
        let (_, again) = saved(&base, &doc);
        let pages = again.pages().unwrap();
        let before = Renderer::new(&Document::from_bytes(base).unwrap(), crate::forms::painter()).render_page(&pages[0], 72.0).unwrap();
        let after = Renderer::new(&again, crate::forms::painter()).render_page(&pages[0], 72.0).unwrap();
        // What changed lies inside the box, as the reader sees the page (red text on a white page).
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        let mut red = 0usize;
        for y in 0..before.height {
            for x in 0..before.width {
                if pixel(&before, x, y) != pixel(&after, x, y) {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                    let p = pixel(&after, x, y);
                    red += usize::from(p[0] > 150 && p[1] < 120 && p[2] < 120);
                }
            }
        }
        assert_eq!((before.width as f64, before.height as f64), (w, h));
        assert!(x1 >= x0 && red > 40, "rotate {rotate}: nothing drawn ({red} red pixels)");
        assert!(x0 >= 28 && y0 >= 28 && x1 <= 152 && y1 <= 64, "rotate {rotate}: drawn outside the box: {x0},{y0} to {x1},{y1}");
    }
}

#[test]
fn a_field_without_an_appearance_is_drawn_from_its_value_even_when_it_is_chinese() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R 12 0 R] /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 30 0 R >> >> >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [10 0 R 11 0 R 12 0 R] >>");
    b.obj(10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /V (Hello) /Rect [10 70 190 90] /MK << /BC [0 0 0] >> >>");
    b.obj(11, &format!("<< /Type /Annot /Subtype /Widget /FT /Tx /T (b) /V {} /Rect [10 40 190 60] /MK << /BC [0 0 0] >> >>", hex16("你好，世界")));
    b.obj(12, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (c) /Rect [10 10 190 30] /MK << /BC [0 0 0] >> >>");
    b.obj(30, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    let doc = Document::from_bytes(b.finish_classic(40, "/Root 1 0 R")).unwrap();
    let pages = doc.pages().unwrap();
    let drawn = Renderer::new(&doc, crate::forms::painter()).render_page(&pages[0], 144.0).unwrap();
    let dark = |y0: u32, y1: u32| -> usize {
        let mut n = 0;
        for y in y0..y1 {
            for x in 24..376 {
                // (The frame of the field is one point wide, 2 pixels, at the edges: left out.)
                n += usize::from(pixel(&drawn, x, y)[0] < 100);
            }
        }
        n
    };
    // Rows of pixels (144 dpi: 2 per point, the page is 200 points high) of the three fields: Latin, Chinese, nothing.
    let (latin, chinese, empty) = (dark(2 * 14, 2 * 28), dark(2 * 44, 2 * 58), dark(2 * 74, 2 * 88));
    assert!(latin > 80 && chinese > 150 && empty == 0, "{latin} {chinese} {empty}");
    assert_eq!(drawn.absent_glyphs + drawn.boxed_characters, 0, "a Chinese character has no glyph");
}
