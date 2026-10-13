//! Layer 5d: PDF to Excel. Files made by hand (a bank statement with no lines round it and Chinese headings, a ruled table, a page with
//! no table), so that what should be found is known; and the damaged and hostile files of the corpus, which must end in a file or a
//! clear error.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;
mod package;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use package::{unzip, well_formed};
use qingpdf_base::testutil::PdfBuilder;
use qingpdf_core::{Document, Error, xlsx};

// --- a PDF with Chinese in it, made by hand ------------------------------------------------------------------------------------

/// Chinese text for a test file: a font (object 7, `F3`) whose character codes are the positions of the characters in `used`, with a
/// ToUnicode table. Not embedded: the page is drawn with whatever the reader has, which a text test does not need.
struct Cjk {
    used: Vec<char>,
}

impl Cjk {
    fn new(texts: &[&str]) -> Cjk {
        let mut used: Vec<char> = Vec::new();
        for c in texts.iter().flat_map(|t| t.chars()) {
            if !used.contains(&c) {
                used.push(c);
            }
        }
        Cjk { used }
    }

    fn hex(&self, text: &str) -> String {
        text.chars().map(|c| format!("{:04X}", self.used.iter().position(|u| *u == c).unwrap() + 1)).collect()
    }

    fn objects(&self, b: &mut PdfBuilder) {
        b.obj(7, "<< /Type /Font /Subtype /Type0 /BaseFont /SimSun /Encoding /Identity-H /DescendantFonts [8 0 R] /ToUnicode 9 0 R >>");
        b.obj(8, "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /SimSun /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>");
        let mut cmap = String::from("/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange\n");
        cmap.push_str(&format!("{} beginbfchar\n", self.used.len()));
        for (i, c) in self.used.iter().enumerate() {
            cmap.push_str(&format!("<{:04X}> <{:04X}>\n", i + 1, *c as u32));
        }
        cmap.push_str("endbfchar endcmap CMapName currentdict /CMap defineresource pop end end");
        b.stream_obj(9, "", cmap.as_bytes());
    }
}

/// How wide Helvetica makes `s` at `size` points (digits, separators and signs; enough for amounts).
fn width(s: &str, size: f32) -> f32 {
    s.chars().map(|c| match c { ',' | '.' => 0.278, '-' | '(' | ')' => 0.333, '+' => 0.584, _ => 0.556 }).sum::<f32>() * size
}

/// The drawing of the statement: Helvetica (`F1`) for what is ASCII, the Chinese font (`F3`) for the rest.
struct Draw<'a> {
    out: String,
    cjk: &'a Cjk,
}

impl Draw<'_> {
    fn at(&mut self, x: f32, y: f32, size: f32, text: &str) {
        if text.is_ascii() {
            self.out.push_str(&format!("BT /F1 {size} Tf {x} {y} Td ({}) Tj ET\n", text.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)")));
        } else {
            self.out.push_str(&format!("BT /F3 {size} Tf {x} {y} Td <{}> Tj ET\n", self.cjk.hex(text)));
        }
    }

    /// Right-aligned to `right`.
    fn ending_at(&mut self, right: f32, y: f32, size: f32, text: &str) {
        self.at(right - width(text, size), y, size, text);
    }
}

fn page_pdf(content: &str, cjk: &Cjk, size: (u32, u32)) -> Vec<u8> {
    pages_pdf(&[content.to_string()], cjk, size)
}

/// A file with a page for each of `contents`, which draw with Helvetica (`F1`), Helvetica-Bold (`F2`) and the Chinese font (`F3`).
fn pages_pdf(contents: &[String], cjk: &Cjk, size: (u32, u32)) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..contents.len()).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", contents.len()));
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    cjk.objects(&mut b);
    for (i, content) in contents.iter().enumerate() {
        b.obj(10 + 2 * i as u32, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R >> >> /Contents {} 0 R >>", size.0, size.1, 11 + 2 * i));
        b.stream_obj(11 + 2 * i as u32, "", content.as_bytes());
    }
    b.finish_classic(10 + 2 * contents.len() as u32, "/Root 1 0 R")
}

const HEAD: [&str; 6] = ["交易日期", "摘要", "交易金额", "账户余额", "对方账号", "编号"];
const MEMOS: [&str; 4] = ["工资收入", "转账支出", "餐饮消费", "利息收入"];

/// A bank statement with no lines round it: a title, a heading row, and `n` rows of date, memo, amount (right-aligned; some with a sign
/// or in brackets), balance, the other party's account (19 digits), and a code with zeros at the front; and a note under it.
fn statement(n: usize) -> Vec<u8> {
    statement_pages(1, n)
}

fn statement_cjk() -> Cjk {
    Cjk::new(&[&HEAD.concat(), &MEMOS.concat(), "银行交易明细", "以上数据仅供参考，请以银行记录为准", "2024年5月1日"])
}

/// `pages` pages of the same statement of `n` rows.
fn statement_pages(pages: usize, n: usize) -> Vec<u8> {
    let cjk = statement_cjk();
    let page = statement_content(&cjk, n);
    pages_pdf(&vec![page; pages], &cjk, (700, 842))
}

fn statement_content(cjk: &Cjk, n: usize) -> String {
    let mut d = Draw { out: String::new(), cjk };
    d.at(200.0, 780.0, 16.0, "银行交易明细");
    // Left edges of the date, the memo, the account and the code; right edges of the amount and the balance.
    let size = 9.0;
    let y0 = 740.0;
    d.at(50.0, y0, size, HEAD[0]);
    d.at(150.0, y0, size, HEAD[1]);
    d.at(370.0 - 36.0, y0, size, HEAD[2]);
    d.at(460.0 - 36.0, y0, size, HEAD[3]);
    d.at(470.0, y0, size, HEAD[4]);
    d.at(600.0, y0, size, HEAD[5]);
    let mut y = y0;
    for i in 0..n {
        y -= 14.0;
        let (day, month) = (1 + i % 28, 1 + (i / 28) % 12);
        let date = if i == 3 { "2024年5月1日".to_string() } else { format!("2024-{month:02}-{day:02}") };
        let amount = match i % 3 {
            0 => format!("+{},{:03}.00", 1 + i, 200 + i),
            1 => format!("-{}.50", 10 + i),
            _ => format!("({},000.00)", 1 + i),
        };
        let balance = format!("{},{:03}.25", 20 + i, i);
        d.at(50.0, y, size, &date);
        d.at(150.0, y, size, MEMOS[i % 4]);
        d.ending_at(370.0, y, size, &amount);
        d.ending_at(460.0, y, size, &balance);
        d.at(470.0, y, size, "6222021234567890123");
        d.at(600.0, y, size, &format!("{:05}", i + 1));
    }
    d.at(50.0, y - 60.0, 9.0, "以上数据仅供参考，请以银行记录为准");
    d.out
}

// --- a reader for what the writer wrote ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Cell {
    Text(String),
    /// A number and the code of its number format.
    Number(f64, String),
}

struct Xlsx {
    parts: Vec<(String, Vec<u8>)>,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}

/// The value of attribute `name` in a tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let at = tag.find(&key)? + key.len();
    Some(&tag[at..at + tag[at..].find('"')?])
}

impl Xlsx {
    fn part(&self, name: &str) -> &str {
        std::str::from_utf8(&self.parts.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no {name}")).1).unwrap()
    }

    fn sheet_names(&self) -> Vec<String> {
        self.part("xl/workbook.xml").split("<sheet ").skip(1).map(|t| unescape(attr(&format!(" {t}"), "name").unwrap())).collect()
    }

    fn strings(&self) -> Vec<String> {
        self.part("xl/sharedStrings.xml").split("<si>").skip(1).map(|si| unescape(&si[si.find('>').unwrap() + 1..si.find("</t>").unwrap()])).collect()
    }

    /// The number format code of each cell format.
    fn format_codes(&self) -> Vec<String> {
        let styles = self.part("xl/styles.xml");
        let codes: BTreeMap<usize, String> = styles
            .split("<numFmt ")
            .skip(1)
            .map(|t| {
                let t = format!(" {t}");
                (attr(&t, "numFmtId").unwrap().parse().unwrap(), unescape(attr(&t, "formatCode").unwrap()))
            })
            .collect();
        let xfs = &styles[styles.find("<cellXfs").unwrap()..styles.find("</cellXfs>").unwrap()];
        xfs.split("<xf ").skip(1).map(|x| codes.get(&attr(&format!(" {x}"), "numFmtId").unwrap().parse().unwrap()).cloned().unwrap_or_else(|| "General".to_string())).collect()
    }

    /// The number of rows and columns of sheet `n` (from 1), and its cells by (row, column) from 0.
    fn cells(&self, n: usize) -> ((usize, usize), BTreeMap<(usize, usize), Cell>) {
        let xml = self.part(&format!("xl/worksheets/sheet{n}.xml"));
        let (strings, codes) = (self.strings(), self.format_codes());
        let dim = attr(&xml[xml.find("<dimension").unwrap() - 1..], "ref").unwrap();
        let last = dim.split(':').next_back().unwrap();
        let letters: String = last.chars().take_while(char::is_ascii_alphabetic).collect();
        let col = |l: &str| l.bytes().fold(0usize, |n, b| n * 26 + usize::from(b - b'A' + 1)) - 1;
        let size = (last[letters.len()..].parse().unwrap(), col(&letters) + 1);
        let mut cells = BTreeMap::new();
        for c in xml.split("<c ").skip(1) {
            let tag = &c[..c.find('>').unwrap()];
            let tag = format!(" {tag}");
            let r = attr(&tag, "r").unwrap();
            let l: String = r.chars().take_while(char::is_ascii_alphabetic).collect();
            let key = (r[l.len()..].parse::<usize>().unwrap() - 1, col(&l));
            let v = &c[c.find("<v>").unwrap() + 3..c.find("</v>").unwrap()];
            let cell = if attr(&tag, "t") == Some("s") {
                Cell::Text(strings[v.parse::<usize>().unwrap()].clone())
            } else {
                let style: usize = attr(&tag, "s").map_or(0, |s| s.parse().unwrap());
                Cell::Number(v.parse().unwrap(), codes[style].clone())
            };
            assert!(cells.insert(key, cell).is_none(), "{r} twice");
        }
        (size, cells)
    }
}

fn check_package(parts: &[(String, Vec<u8>)]) {
    // Every XML part is well formed, every part has a content type, every relationship points at a part.
    for (name, data) in parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            well_formed(std::str::from_utf8(data).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
    let x = Xlsx { parts: parts.to_vec() };
    let types = x.part("[Content_Types].xml");
    for (name, _) in parts {
        let ext = name.rsplit('.').next().unwrap();
        assert!(name == "[Content_Types].xml" || types.contains(&format!("PartName=\"/{name}\"")) || types.contains(&format!("Extension=\"{ext}\"")), "{name} has no content type");
    }
    for rels in ["_rels/.rels", "xl/_rels/workbook.xml.rels"] {
        let base = if rels.starts_with("xl") { "xl/" } else { "" };
        for target in x.part(rels).split("Target=\"").skip(1) {
            let target = &target[..target.find('"').unwrap()];
            assert!(parts.iter().any(|(n, _)| *n == format!("{base}{target}")), "{rels}: {target} is not in the package");
        }
    }
    // Every sheet part is named in the workbook, with the relationship that names the part.
    let rels = x.part("xl/_rels/workbook.xml.rels");
    for (n, _) in parts.iter().filter(|(n, _)| n.starts_with("xl/worksheets/")) {
        assert!(rels.contains(&format!("Target=\"{}\"", n.strip_prefix("xl/").unwrap())), "{n} has no relationship");
        assert!(types.contains(&format!("PartName=\"/{n}\"")), "{n} has no override");
    }
    assert_eq!(x.sheet_names().len(), parts.iter().filter(|(n, _)| n.starts_with("xl/worksheets/")).count());
    // The shared strings are all different, and the sheets use the numbers that there are.
    let strings = x.strings();
    let unique: std::collections::HashSet<&String> = strings.iter().collect();
    assert_eq!(unique.len(), strings.len(), "a string is in the shared strings twice");
    let sst = x.part("xl/sharedStrings.xml");
    assert_eq!(attr(&sst[sst.find("<sst").unwrap()..], "uniqueCount").unwrap().parse::<usize>().unwrap(), strings.len());
    let mut uses = 0usize;
    for (n, _) in parts.iter().filter(|(n, _)| n.starts_with("xl/worksheets/")) {
        for c in x.part(n).split("<c ").skip(1) {
            if c[..c.find('>').unwrap()].contains("t=\"s\"") {
                uses += 1;
                assert!(c[c.find("<v>").unwrap() + 3..c.find("</v>").unwrap()].parse::<usize>().unwrap() < strings.len());
            }
        }
    }
    assert_eq!(attr(&sst[sst.find("<sst").unwrap()..], "count").unwrap().parse::<usize>().unwrap(), uses);
}

fn convert(pdf: Vec<u8>) -> (Xlsx, xlsx::Report) {
    let doc = Document::from_bytes(pdf).unwrap();
    let mut out = Vec::new();
    let report = xlsx::export(&doc, "", None, &mut out, None).unwrap();
    assert_eq!(report.bytes as usize, out.len());
    let parts = unzip(&out);
    check_package(&parts);
    (Xlsx { parts }, report)
}

fn text(c: &Cell) -> &str {
    match c {
        Cell::Text(t) => t,
        Cell::Number(..) => panic!("{c:?} is a number"),
    }
}

// --- tests -------------------------------------------------------------------------------------------------------------------

#[test]
fn a_statement_with_no_lines_is_a_sheet_with_numbers_dates_and_text() {
    let pdf = statement(40);
    // (To look at the file with other tools: set QINGPDF_TEST_OUT to a folder.)
    if let Ok(dir) = std::env::var("QINGPDF_TEST_OUT") {
        std::fs::write(std::path::Path::new(&dir).join("statement.pdf"), &pdf).unwrap();
    }
    let (x, report) = convert(pdf);
    assert_eq!((report.pages, report.tables), (1, 1));
    assert_eq!(x.sheet_names(), ["第1页-表1"]);
    let ((rows, cols), cells) = x.cells(1);
    // The heading and forty rows of six columns; the title above it and the note below are not part of the table.
    assert_eq!((rows, cols), (41, 6), "{:?}", cells.iter().take(8).collect::<Vec<_>>());
    let missing: Vec<(usize, usize)> = (0..41).flat_map(|r| (0..6).map(move |c| (r, c))).filter(|k| !cells.contains_key(k)).collect();
    assert_eq!(report.cells, 41 * 6, "missing {missing:?}");
    for (c, h) in HEAD.iter().enumerate() {
        assert_eq!(text(&cells[&(0, c)]), *h);
    }
    // Dates are dates (2024-01-01 is 45292), the amounts and balances are numbers, in the formats they were written in.
    assert_eq!(cells[&(1, 0)], Cell::Number(45292.0, "yyyy-mm-dd".to_string()));
    assert_eq!(cells[&(2, 0)], Cell::Number(45293.0, "yyyy-mm-dd".to_string()));
    assert_eq!(cells[&(4, 0)], Cell::Number(45413.0, "yyyy\"年\"m\"月\"d\"日\"".to_string()));
    assert_eq!(cells[&(1, 2)], Cell::Number(1200.0, "#,##0.00".to_string()));
    assert_eq!(cells[&(2, 2)], Cell::Number(-11.5, "0.00".to_string()));
    assert_eq!(cells[&(3, 2)], Cell::Number(-3000.0, "#,##0.00_);(#,##0.00)".to_string()));
    assert_eq!(cells[&(1, 3)], Cell::Number(20000.25, "#,##0.00".to_string()));
    // The account of the other party, with 19 digits, and the code with zeros at the front, stay text.
    for r in 1..=40 {
        assert_eq!(text(&cells[&(r, 4)]), "6222021234567890123");
        assert_eq!(text(&cells[&(r, 5)]), format!("{r:05}"));
        assert!(matches!(cells[&(r, 0)], Cell::Number(..)) && matches!(cells[&(r, 2)], Cell::Number(..)) && matches!(cells[&(r, 3)], Cell::Number(..)));
        assert_eq!(text(&cells[&(r, 1)]), MEMOS[(r - 1) % 4]);
    }
    // The 19-digit account is one shared string, however many rows have it.
    assert_eq!(x.strings().iter().filter(|s| *s == "6222021234567890123").count(), 1);
}

#[test]
fn a_ruled_table_keeps_its_merged_cells_and_the_text_goes_in_the_right_cells() {
    let cjk = Cjk::new(&["项目", "数量", "合计"]);
    let mut d = Draw { out: String::new(), cjk: &cjk };
    // A grid of 3 rows by 3 columns: x = 100, 200, 300, 400; y = 700, 670, 640, 610; the line between the second and third columns
    // is missing in the first row (the heading is across them).
    d.out.push_str("0.5 w\n");
    for y in [700.0, 670.0, 640.0, 610.0] {
        d.out.push_str(&format!("100 {y} m 400 {y} l S\n"));
    }
    d.out.push_str("100 700 m 100 610 l S 200 700 m 200 610 l S 300 670 m 300 610 l S 400 700 m 400 610 l S
");
    d.at(110.0, 680.0, 10.0, "项目");
    d.at(210.0, 680.0, 10.0, "数量");
    d.at(110.0, 650.0, 10.0, "apple");
    d.at(210.0, 650.0, 10.0, "12");
    d.at(310.0, 650.0, 10.0, "3.50");
    d.at(110.0, 620.0, 10.0, "合计");
    d.at(210.0, 620.0, 10.0, "12");
    d.at(310.0, 620.0, 10.0, "42.00");
    let (x, report) = convert(page_pdf(&d.out, &cjk, (595, 842)));
    assert_eq!(report.tables, 1);
    let ((rows, cols), cells) = x.cells(1);
    assert_eq!((rows, cols), (3, 3));
    assert_eq!(text(&cells[&(0, 0)]), "项目");
    assert_eq!(text(&cells[&(0, 1)]), "数量");
    assert_eq!(cells[&(1, 1)], Cell::Number(12.0, "General".to_string()));
    assert_eq!(cells[&(1, 2)], Cell::Number(3.5, "0.00".to_string()));
    assert_eq!(cells[&(2, 2)], Cell::Number(42.0, "0.00".to_string()));
    assert_eq!(text(&cells[&(2, 0)]), "合计");
    // The heading is across the second and third columns: a merged range, and nothing in the cell it covers.
    assert!(x.part("xl/worksheets/sheet1.xml").contains("<mergeCells count=\"1\"><mergeCell ref=\"B1:C1\"/></mergeCells>"), "{}", x.part("xl/worksheets/sheet1.xml"));
    assert!(!cells.contains_key(&(0, 2)));
}

#[test]
fn a_file_with_no_table_has_its_text_in_one_sheet_and_a_warning() {
    let cjk = Cjk::new(&["一段普通的文字"]);
    let mut d = Draw { out: String::new(), cjk: &cjk };
    d.at(72.0, 700.0, 12.0, "A paragraph of plain text & <more>.");
    d.at(72.0, 684.0, 12.0, "007");
    d.at(72.0, 668.0, 12.0, "一段普通的文字");
    let (x, report) = convert(page_pdf(&d.out, &cjk, (595, 842)));
    assert_eq!((report.tables, report.pages), (0, 1));
    assert!(report.warnings.iter().any(|w| w.contains("no table")), "{:?}", report.warnings);
    assert_eq!(x.sheet_names(), ["文字"]);
    let (_, cells) = x.cells(1);
    assert_eq!(cells.values().map(text).collect::<Vec<_>>(), ["A paragraph of plain text & <more>.", "007", "一段普通的文字"]);
}

#[test]
fn every_table_of_every_page_is_a_sheet_with_a_name_of_its_own() {
    // Three pages of the same statement, and one of plain text between them that gets no sheet.
    let one = statement(8);
    let doc = Document::from_bytes(one.clone()).unwrap();
    let mut out = Vec::new();
    let report = xlsx::export(&doc, "", Some(&[0, 0, 0]), &mut out, None).unwrap();
    assert_eq!(report.tables, 3);
    let x = Xlsx { parts: unzip(&out) };
    check_package(&x.parts);
    assert_eq!(x.sheet_names(), ["第1页-表1", "第1页-表1(2)", "第1页-表1(3)"]);
    for name in x.sheet_names() {
        assert!(name.encode_utf16().count() <= 31 && !name.contains(['[', ']', ':', '*', '?', '/', '\\']));
    }
}

#[test]
fn text_is_escaped_and_what_xml_does_not_allow_is_dropped() {
    let cjk = Cjk::new(&["x"]);
    let mut d = Draw { out: String::new(), cjk: &cjk };
    // Three columns of five rows with markup, quotes and a control character (0x01, written as an octal escape in the string).
    for r in 0..5 {
        let y = 700.0 - 14.0 * r as f32;
        d.at(50.0, y, 9.0, "<b>&\"q\"");
        d.out.push_str(&format!("BT /F1 9 Tf 200 {y} Td (a\\001b & c) Tj ET\n"));
        d.at(350.0, y, 9.0, &format!("{r}"));
    }
    let (x, report) = convert(page_pdf(&d.out, &cjk, (595, 842)));
    assert_eq!(report.tables, 1);
    let (_, cells) = x.cells(1);
    assert_eq!(text(&cells[&(0, 0)]), "<b>&\"q\"");
    assert!(x.part("xl/sharedStrings.xml").contains("&lt;b&gt;&amp;\"q\""));
    assert!(!text(&cells[&(0, 1)]).contains('\u{1}'));
}

#[test]
fn cancel_stops_the_conversion_and_a_missing_page_is_an_error() {
    let doc = Document::from_bytes(statement(5)).unwrap();
    let stop = Arc::new(AtomicBool::new(true));
    assert!(matches!(xlsx::export(&doc, "", None, &mut Vec::new(), Some(stop)), Err(Error::Cancelled)));
    assert!(matches!(xlsx::export(&doc, "", Some(&[7]), &mut Vec::new(), None), Err(Error::Invalid(_))));
}

#[test]
fn a_page_of_a_hundred_thousand_lines_and_four_hundred_thousand_characters_is_converted_fast() {
    // 100,000 lines of 4 characters in 8 columns of 12,500 rows: far more tables than a page may give, each as long as one may be.
    let cjk = Cjk::new(&["x"]);
    let mut s = String::with_capacity(5_000_000);
    for i in 0..100_000usize {
        let (r, c) = (i / 8, i % 8);
        s.push_str(&format!("BT /F1 4 Tf {} {} Td (a{:03}) Tj ET\n", 10 + 30 * c, 62_990 - 5 * r, i % 1000));
    }
    let pdf = page_pdf(&s, &cjk, (300, 63_000));
    let t = std::time::Instant::now();
    let (_, report) = convert(pdf);
    let took = t.elapsed();
    eprintln!("100,000 lines: {took:?}, {} tables, {} cells", report.tables, report.cells);
    assert!(report.tables >= 1 && report.tables <= 64, "{}", report.tables);
    assert!(took < std::time::Duration::from_secs(8), "{took:?}");
}

#[test]
fn more_tables_than_a_workbook_may_have_end_the_conversion_with_a_limit() {
    let doc = Document::from_bytes(statement_pages(xlsx::MAX_SHEETS + 1, 4)).unwrap();
    let mut out = Vec::new();
    let Err(Error::Limit(m)) = xlsx::export(&doc, "", None, &mut out, None) else { panic!("no limit") };
    assert!(m.contains("tables"), "{m}");
    // The first thousand pages are a workbook all the same.
    let report = xlsx::export(&doc, "", Some(&(0..xlsx::MAX_SHEETS).collect::<Vec<_>>()), &mut Vec::new(), None).unwrap();
    assert_eq!(report.tables, xlsx::MAX_SHEETS);
}

#[test]
fn a_hundred_pages_of_a_statement_take_no_more_than_three_seconds() {
    let pdf = statement_pages(100, 45);
    let t = std::time::Instant::now();
    let (x, report) = convert(pdf);
    let took = t.elapsed();
    eprintln!("100 pages: {took:?} (with the checks of the package), {} bytes", report.bytes);
    // The statement goes on from page to page (the heading, written on every page, is one row): one sheet.
    assert_eq!((report.pages, report.tables, report.cells), (100, 1, (1 + 100 * 45) * 6));
    assert_eq!(x.sheet_names(), ["第1-100页-表1"]);
    assert!(took.as_secs_f64() <= 3.0, "{took:?}");
}

#[test]
fn the_damaged_and_encrypted_files_of_the_corpus_give_a_file_or_a_clear_error() {
    let mut checked = 0;
    for dir in ["public/damaged", "public/encrypted"] {
        for path in common::walk_pdfs(&common::corpus_root().join(dir)) {
            let Ok(doc) = Document::open(&path) else { continue };
            let mut out = Vec::new();
            match xlsx::export(&doc, "", None, &mut out, None) {
                Ok(_) => check_package(&unzip(&out)),
                Err(e) => assert!(!e.to_string().is_empty(), "{}", path.display()),
            }
            checked += 1;
        }
    }
    eprintln!("{checked} files");
}

// --- a table that goes on over pages; whole numbers in brackets ---------------------------------------------------------------

/// A page of a statement with no lines: a running header, `rows` of date, memo, amount and balance from y = `top` (the heading first
/// if `heading`), a note under the table if `note`, and a page number in the footer. `shift` moves the amount and the balance.
struct Leaf {
    heading: bool,
    rows: std::ops::Range<usize>,
    top: f32,
    shift: f32,
    note: bool,
}

fn leaf(heading: bool, rows: std::ops::Range<usize>) -> Leaf {
    Leaf { heading, rows, top: 770.0, shift: 0.0, note: false }
}

fn ledger(leaves: &[Leaf]) -> Vec<u8> {
    let cjk = Cjk::new(&["x"]);
    let contents: Vec<String> = leaves
        .iter()
        .enumerate()
        .map(|(n, l)| {
            let mut d = Draw { out: String::new(), cjk: &cjk };
            d.at(50.0, 810.0, 8.0, &format!("Account 6222 0212 - statement - page {}", n + 1));
            let mut y = l.top;
            if l.heading {
                d.at(50.0, y, 9.0, "Date");
                d.at(150.0, y, 9.0, "Memo");
                d.ending_at(370.0 + l.shift, y, 9.0, "Amount");
                d.ending_at(470.0 + l.shift, y, 9.0, "Balance");
                y -= 14.0;
            }
            for i in l.rows.clone() {
                d.at(50.0, y, 9.0, &format!("2024-05-{:02}", i % 28 + 1));
                d.at(150.0, y, 9.0, &format!("Payment {i}"));
                d.ending_at(370.0 + l.shift, y, 9.0, &format!("{},{:03}.50", 1 + i, 100 + i));
                d.ending_at(470.0 + l.shift, y, 9.0, &format!("{},{:03}.25", 20 + i, i));
                y -= 14.0;
            }
            if l.note {
                d.at(50.0, y - 30.0, 9.0, "End of the statement");
            }
            d.at(260.0, 30.0, 8.0, &format!("Page {}", n + 1));
            d.out
        })
        .collect();
    pages_pdf(&contents, &cjk, (595, 842))
}

fn sheets_of(pdf: Vec<u8>, pages: Option<&[usize]>) -> (Xlsx, xlsx::Report) {
    let doc = Document::from_bytes(pdf).unwrap();
    let mut out = Vec::new();
    let report = xlsx::export(&doc, "", pages, &mut out, None).unwrap();
    let parts = unzip(&out);
    check_package(&parts);
    (Xlsx { parts }, report)
}

#[test]
fn a_statement_over_three_pages_is_one_sheet_with_or_without_the_heading_on_each_page() {
    for each in [false, true] {
        let pdf = ledger(&[leaf(true, 0..40), leaf(each, 40..80), leaf(each, 80..120)]);
        let (x, report) = sheets_of(pdf, None);
        assert_eq!((report.pages, report.tables, report.cells), (3, 1, 121 * 4), "heading on each page: {each}");
        assert_eq!(x.sheet_names(), ["第1-3页-表1"]);
        let ((rows, cols), cells) = x.cells(1);
        assert_eq!((rows, cols), (121, 4));
        assert_eq!(text(&cells[&(0, 0)]), "Date");
        // The heading of the pages 2 and 3 is no row (the rows go on from 40 to 120); the page numbers and the running header are no cells.
        for r in [1, 40, 41, 80, 81, 120] {
            assert_eq!(text(&cells[&(r, 1)]), format!("Payment {}", r - 1), "row {r}");
        }
        assert_eq!(cells[&(41, 2)], Cell::Number(41_140.5, "#,##0.00".to_string()));
        assert!(cells.values().all(|c| !matches!(c, Cell::Text(t) if t.starts_with("Account") || t.starts_with("Page"))));
    }
}

#[test]
fn a_table_does_not_go_on_when_the_columns_move_when_a_note_follows_it_or_when_the_next_one_starts_low() {
    let names = |leaves: &[Leaf], pages: Option<&[usize]>| sheets_of(ledger(leaves), pages).0.sheet_names();
    // The amount and the balance of the second page are 60 points to the right: its own table, and the third does not go on from it.
    let mut moved = leaf(false, 40..80);
    moved.shift = 60.0;
    assert_eq!(names(&[leaf(true, 0..40), moved, leaf(false, 80..120)], None), ["第1页-表1", "第2页-表1", "第3页-表1"]);
    // A note under the table of the first page ends it; the next two pages are one.
    let mut noted = leaf(true, 0..40);
    noted.note = true;
    assert_eq!(names(&[noted, leaf(false, 40..80), leaf(false, 80..120)], None), ["第1页-表1", "第2-3页-表1"]);
    // The second table begins low on its page (below a heading of its own, say).
    let mut low = leaf(false, 40..60);
    low.top = 500.0;
    assert_eq!(names(&[leaf(true, 0..40), low], None), ["第1页-表1", "第2页-表1"]);
    // A table that ends in the lower half of its page, with nothing under it, goes on; one that ends in the upper half does not.
    assert_eq!(names(&[leaf(true, 0..28), leaf(false, 28..56)], None), ["第1-2页-表1"]);
    assert_eq!(names(&[leaf(true, 0..15), leaf(false, 15..30)], None), ["第1页-表1", "第2页-表1"]);
    // Pages that do not follow each other (1 and 3) are two tables.
    assert_eq!(names(&[leaf(true, 0..40), leaf(false, 40..80), leaf(false, 80..120)], Some(&[0, 2])), ["第1页-表1", "第3页-表1"]);
    // A page between them with no table ends it.
    assert_eq!(names(&[leaf(true, 0..40), leaf(false, 0..0), leaf(false, 40..80)], None), ["第1页-表1", "第3页-表1"]);
}

#[test]
fn whole_numbers_in_brackets_are_list_numbers_unless_the_column_is_made_of_amounts() {
    let cjk = Cjk::new(&["x"]);
    let mut d = Draw { out: String::new(), cjk: &cjk };
    // Column A: list numbers. C: amounts, two of them negative whole numbers (bracketed, minus after). D: whole numbers only.
    d.at(50.0, 700.0, 9.0, "No");
    d.at(150.0, 700.0, 9.0, "Item");
    d.ending_at(400.0, 700.0, 9.0, "Amount");
    d.ending_at(480.0, 700.0, 9.0, "Count");
    let amounts = ["10.50", "20.00", "(3)", "5.25", "7-", "1,200.00", "8.10", "9.99"];
    let counts = ["100", "(50)", "200", "(30)", "300", "(20)", "4-", "500"];
    for i in 0..8 {
        let y = 686.0 - 14.0 * i as f32;
        d.at(50.0, y, 9.0, &format!("({})", i + 1));
        d.at(150.0, y, 9.0, &format!("Item number {i}"));
        d.ending_at(400.0, y, 9.0, amounts[i]);
        d.ending_at(480.0, y, 9.0, counts[i]);
    }
    let (x, report) = convert(page_pdf(&d.out, &cjk, (595, 842)));
    assert_eq!(report.tables, 1);
    let (_, cells) = x.cells(1);
    for i in 0..8 {
        assert_eq!(text(&cells[&(i + 1, 0)]), format!("({})", i + 1), "a list number stays text");
    }
    // In the column of amounts the two are negative numbers; in the column of whole numbers they are text.
    assert_eq!(cells[&(3, 2)], Cell::Number(-3.0, "0_);(0)".to_string()));
    assert_eq!(cells[&(5, 2)], Cell::Number(-7.0, "General".to_string()));
    assert_eq!(cells[&(1, 2)], Cell::Number(10.5, "0.00".to_string()));
    assert_eq!(cells[&(1, 3)], Cell::Number(100.0, "General".to_string()));
    for (r, t) in [(2, "(50)"), (4, "(30)"), (6, "(20)"), (7, "4-")] {
        assert_eq!(text(&cells[&(r, 3)]), t);
    }
}
