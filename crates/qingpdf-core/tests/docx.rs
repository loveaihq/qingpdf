//! Layer 5b: PDF to Word. Small files made by hand, so that what the layout analysis should find is known; and the files of the
//! corpus that are damaged or hostile, which must end in a file or a clear error.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;
mod package;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use qingpdf_base::testutil::PdfBuilder;
use qingpdf_core::docx;
use package::{unzip, well_formed};
use qingpdf_core::{Document, Error};

struct Docx {
    parts: Vec<(String, Vec<u8>)>,
}

impl Docx {
    fn part(&self, name: &str) -> &str {
        std::str::from_utf8(&self.parts.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no {name}")).1).unwrap()
    }

    fn document(&self) -> &str {
        self.part("word/document.xml")
    }

    /// The text of the paragraphs of the body (a paragraph on each line; `<w:tab/>` is a tab).
    fn text(&self) -> String {
        let doc = self.document();
        let mut out = String::new();
        let mut rest = doc;
        while let Some(i) = rest.find('<') {
            rest = &rest[i..];
            if rest.starts_with("<w:t ") || rest.starts_with("<w:t>") {
                let s = rest.find('>').unwrap() + 1;
                let e = rest.find("</w:t>").unwrap();
                out.push_str(&rest[s..e].replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"));
                rest = &rest[e..];
            } else if rest.starts_with("</w:p>") {
                out.push('\n');
            }
            rest = &rest[1..];
        }
        out
    }
}

fn convert(pdf: Vec<u8>) -> (Docx, docx::Report) {
    let doc = Document::from_bytes(pdf).unwrap();
    let mut out = Vec::new();
    let report = docx::export(&doc, "", None, &mut out, None).unwrap();
    assert_eq!(report.bytes as usize, out.len());
    let parts = unzip(&out);
    // Every XML part is well formed, every part has a content type, every relationship points at a part.
    for (name, data) in &parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            well_formed(std::str::from_utf8(data).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
    let docx = Docx { parts };
    let types = docx.part("[Content_Types].xml");
    for (name, _) in &docx.parts {
        let ext = name.rsplit('.').next().unwrap();
        assert!(name == "[Content_Types].xml" || types.contains(&format!("PartName=\"/{name}\"")) || types.contains(&format!("Extension=\"{ext}\"")), "{name} has no content type");
    }
    for rels in ["_rels/.rels", "word/_rels/document.xml.rels"] {
        let base = if rels.starts_with("word") { "word/" } else { "" };
        for target in docx.part(rels).split("Target=\"").skip(1) {
            let target = &target[..target.find('"').unwrap()];
            assert!(docx.parts.iter().any(|(n, _)| *n == format!("{base}{target}")), "{rels}: {target} is not in the package");
        }
    }
    (docx, report)
}

/// A one-page file with `content` drawn in Helvetica (F1) and Helvetica-Bold (F2), with `extra` objects from 7 up.
fn page_with(content: &str, size: (u32, u32), extra: impl FnOnce(&mut PdfBuilder), resources: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 5 0 R /F2 6 0 R >> {resources} >> /Contents 4 0 R >>", size.0, size.1));
    b.stream_obj(4, "", content.as_bytes());
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>");
    extra(&mut b);
    b.finish_classic(20, "/Root 1 0 R")
}

/// Lines of text, one per `Tj`, at the given places.
fn text_at(lines: &[(&str, &str, f32, f32, f32)]) -> String {
    let mut s = String::new();
    for (font, text, size, x, y) in lines {
        s.push_str(&format!("BT /{font} {size} Tf {x} {y} Td ({text}) Tj ET\n"));
    }
    s
}

// --- tests -------------------------------------------------------------------------------------------------------------------

#[test]
fn a_page_of_text_is_a_package_with_paragraphs_and_a_heading() {
    let content = text_at(&[
        ("F2", "A Big Title", 24.0, 72.0, 700.0),
        ("F1", "The first paragraph has a line", 10.0, 72.0, 650.0),
        ("F1", "and a second line of the same.", 10.0, 72.0, 638.0),
        ("F1", "A new paragraph, after a gap.", 10.0, 72.0, 610.0),
        ("F1", "A third one, still more text.", 10.0, 72.0, 598.0),
        ("F1", "One more line at the end.", 10.0, 72.0, 586.0),
    ]);
    let (docx, report) = convert(page_with(&content, (595, 842), |_| {}, ""));
    let names: Vec<&str> = docx.parts.iter().map(|(n, _)| n.as_str()).collect();
    for n in ["[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/styles.xml", "word/numbering.xml", "word/_rels/document.xml.rels", "docProps/core.xml"] {
        assert!(names.contains(&n), "{n} is missing: {names:?}");
    }
    assert_eq!(names[0], "[Content_Types].xml");
    let text = docx.text();
    assert!(text.contains("A Big Title\n"), "{text}");
    assert!(text.contains("The first paragraph has a line and a second line of the same.\n"), "{text}");
    assert!(text.contains("A new paragraph, after a gap. A third one, still more text. One more line at the end."), "{text}");
    assert_eq!((report.pages, report.counts.headings, report.counts.tables, report.counts.images), (1, 1, 0, 0));
    assert!(docx.document().contains("<w:pStyle w:val=\"Heading1\"/>"));
    assert!(docx.document().trim_end().ends_with("</w:sectPr></w:body></w:document>"));
    assert!(docx.document().contains("<w:pgSz w:w=\"11900\" w:h=\"16840\"/>"));
}

#[test]
fn a_bold_line_of_the_size_of_the_text_is_a_heading_and_text_in_a_bigger_size_is_not() {
    // Four paragraphs of three lines in 12 points, each after a bold line of 12 points (the headings of a gongwen are in a Hei face
    // of the size of the text), and a block of references in 14 points: several lines each, so that it is text.
    let mut lines: Vec<(&str, String, f32, f32, f32)> = Vec::new();
    let mut y = 760.0f32;
    for section in 1..=4 {
        lines.push(("F2", format!("Section number {section}"), 12.0, 72.0, y));
        y -= 24.0;
        for k in 0..3 {
            lines.push(("F1", format!("A line of the body text number {k} of section {section}, long enough to fill the line well"), 12.0, 72.0, y));
            y -= 15.0;
        }
        y -= 10.0;
    }
    for r in 0..3 {
        for k in 0..2 {
            lines.push(("F1", format!("[{r}] A reference line {k} in a bigger size, that is text and no heading, and it goes on"), 14.0, 72.0, y));
            y -= 17.0;
        }
        y -= 8.0;
    }
    let borrowed: Vec<(&str, &str, f32, f32, f32)> = lines.iter().map(|(f, t, s, x, y)| (*f, t.as_str(), *s, *x, *y)).collect();
    let (docx, report) = convert(page_with(&text_at(&borrowed), (595, 842), |_| {}, ""));
    assert_eq!(report.counts.headings, 4, "{}", docx.document());
    let doc = docx.document();
    for section in 1..=4 {
        let at = doc.find(&format!("Section number {section}")).unwrap();
        let start = doc[..at].rfind("<w:p>").unwrap();
        assert!(doc[start..at].contains("<w:pStyle w:val=\"Heading"), "section {section}: {}", &doc[start..at]);
    }
    assert_eq!(doc.matches("<w:pStyle w:val=\"Heading").count(), 4);
}

#[test]
fn xml_special_characters_are_escaped() {
    let (docx, _) = convert(page_with("BT /F1 12 Tf 72 700 Td (a<b & c>d) Tj ET", (595, 842), |_| {}, ""));
    assert!(docx.document().contains("a&lt;b &amp; c&gt;d"), "{}", docx.document());
    assert_eq!(docx.text().trim(), "a<b & c>d");
}

#[test]
fn lines_drawn_as_a_grid_make_a_table_with_the_text_in_its_cells() {
    // A grid of 2 rows and 2 columns, 150 points wide, and one word in each cell; a column of text below it.
    let mut c = String::from("0.5 w\n");
    for y in [700, 670, 640] {
        c.push_str(&format!("100 {y} m 400 {y} l S\n"));
    }
    for x in [100, 250, 400] {
        c.push_str(&format!("{x} 640 m {x} 700 l S\n"));
    }
    c.push_str(&text_at(&[("F1", "alpha", 10.0, 110.0, 680.0), ("F1", "beta", 10.0, 260.0, 680.0), ("F1", "gamma", 10.0, 110.0, 650.0), ("F1", "delta", 10.0, 260.0, 650.0), ("F1", "Below the table.", 10.0, 100.0, 600.0)]));
    let (docx, report) = convert(page_with(&c, (595, 842), |_| {}, ""));
    let doc = docx.document();
    assert_eq!(report.counts.tables, 1, "{doc}");
    assert_eq!(doc.matches("<w:tc>").count(), 4);
    assert_eq!(doc.matches("<w:tr>").count(), 2);
    assert_eq!(doc.matches("<w:gridCol ").count(), 2);
    let order: Vec<usize> = ["alpha", "beta", "gamma", "delta", "Below the table."].iter().map(|w| doc.find(w).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    // alpha is in the first cell: between the first <w:tc> and the second.
    let tcs: Vec<usize> = doc.match_indices("<w:tc>").map(|(i, _)| i).collect();
    assert!(tcs[0] < doc.find("alpha").unwrap() && doc.find("alpha").unwrap() < tcs[1] && tcs[1] < doc.find("beta").unwrap());
    assert!(doc.contains("<w:tcBorders><w:top w:val=\"single\""));
}

#[test]
fn text_lined_up_in_columns_with_no_lines_round_it_is_a_table_without_borders() {
    // A statement: a heading row and five rows of three columns (the amounts end at one place), a paragraph above and one below.
    let mut lines: Vec<(&str, String, f32, f32, f32)> = vec![("F1", "Statement of the account for the month".to_string(), 10.0, 72.0, 760.0)];
    for (i, (d, m, a)) in [("Date", "Memo", "Amount"), ("2024-05-01", "Salary", "15,000.00"), ("2024-05-02", "Rent", "-3,200.50"), ("2024-05-03", "Food", "-86.20"), ("2024-05-04", "Interest", "1.25"), ("2024-05-05", "Gift", "(500.00)")].iter().enumerate() {
        let y = 700.0 - 14.0 * i as f32;
        lines.push(("F1", d.to_string(), 9.0, 72.0, y));
        lines.push(("F1", m.to_string(), 9.0, 200.0, y));
        // (Right-aligned by hand: Helvetica digits are 0.556 of the size wide.)
        lines.push(("F1", a.to_string(), 9.0, 400.0 - 5.0 * a.len() as f32, y));
    }
    lines.push(("F1", "End of the statement, with a note under it.".to_string(), 10.0, 72.0, 560.0));
    let borrowed: Vec<(&str, &str, f32, f32, f32)> = lines.iter().map(|(f, t, s, x, y)| (*f, t.as_str(), *s, *x, *y)).collect();
    let (docx, report) = convert(page_with(&text_at(&borrowed), (595, 842), |_| {}, ""));
    let doc = docx.document();
    assert_eq!(report.counts.tables, 1, "{doc}");
    assert_eq!((doc.matches("<w:tr>").count(), doc.matches("<w:tc>").count(), doc.matches("<w:gridCol ").count()), (6, 18, 3));
    // No border on any edge: the page had no lines.
    assert!(!doc.contains("w:val=\"single\""), "{doc}");
    let order: Vec<usize> = ["Statement of the account", "Date", "Salary", "15,000.00", "(500.00)", "End of the statement"].iter().map(|w| doc.find(w).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    // The first row of the table is in the first row of cells: "Memo" is in the second cell, "Amount" in the third.
    let tcs: Vec<usize> = doc.match_indices("<w:tc>").map(|(i, _)| i).collect();
    assert!(tcs[1] < doc.find("Memo").unwrap() && doc.find("Memo").unwrap() < tcs[2] && tcs[2] < doc.find("Amount").unwrap() && doc.find("Amount").unwrap() < tcs[3]);
}

#[test]
fn a_cell_with_no_line_between_it_and_the_next_is_one_cell() {
    // The same grid, without the inner vertical line in the top row.
    let mut c = String::new();
    for y in [700, 670, 640] {
        c.push_str(&format!("100 {y} m 400 {y} l S\n"));
    }
    c.push_str("100 640 m 100 700 l S 400 640 m 400 700 l S 250 640 m 250 670 l S\n");
    c.push_str(&text_at(&[("F1", "wide", 10.0, 110.0, 680.0), ("F1", "left", 10.0, 110.0, 650.0), ("F1", "right", 10.0, 260.0, 650.0)]));
    let (docx, _) = convert(page_with(&c, (595, 842), |_| {}, ""));
    let doc = docx.document();
    assert!(doc.contains("<w:gridSpan w:val=\"2\"/>"), "{doc}");
    assert_eq!(doc.matches("<w:tc>").count(), 3);
}

#[test]
fn two_columns_are_read_one_after_the_other() {
    // Left column first (drawn first), then the right: each line of one column is above the lines of the other.
    let mut lines = Vec::new();
    for (i, t) in ["left one is a longer line", "left two is a longer line", "left three"].iter().enumerate() {
        lines.push(("F1", *t, 10.0, 60.0, 700.0 - 12.0 * i as f32));
    }
    for (i, t) in ["right one is a longer line", "right two is a longer line", "right three"].iter().enumerate() {
        lines.push(("F1", *t, 10.0, 320.0, 700.0 - 12.0 * i as f32));
    }
    // A wide title above both.
    lines.insert(0, ("F1", "A title across the page that is quite long indeed, over both columns of the page", 10.0, 60.0, 740.0));
    let (docx, _) = convert(page_with(&text_at(&lines), (595, 842), |_| {}, ""));
    let text = docx.text();
    let at = |w: &str| text.find(w).unwrap();
    assert!(at("A title") < at("left one") && at("left three") < at("right one"), "{text}");
    assert!(text.contains("left one is a longer line left two is a longer line left three\n"), "{text}");
    assert!(text.contains("right one is a longer line right two is a longer line right three\n"), "{text}");
}

#[test]
fn bullets_and_numbers_become_word_lists() {
    let content = text_at(&[
        ("F1", "A line before the lists, as an intro.", 10.0, 72.0, 700.0),
        ("F1", "- first point", 10.0, 72.0, 670.0),
        ("F1", "- second point", 10.0, 72.0, 658.0),
        ("F1", "3. third", 10.0, 72.0, 600.0),
        ("F1", "4. fourth", 10.0, 72.0, 588.0),
    ]);
    let (docx, _) = convert(page_with(&content, (595, 842), |_| {}, ""));
    let doc = docx.document();
    assert_eq!(doc.matches("<w:numPr>").count(), 4, "{doc}");
    // The markers are not in the text any more; the numbering gives them.
    let text = docx.text();
    assert!(text.contains("first point\n") && text.contains("third\n") && !text.contains("3. third"), "{text}");
    let numbering = docx.part("word/numbering.xml");
    assert!(numbering.contains("<w:startOverride w:val=\"3\"/>") && numbering.contains("<w:lvlText w:val=\"%1.\"/>"), "{numbering}");
}

#[test]
fn a_running_footer_and_the_page_numbers_go_to_the_footer() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
    b.obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for (i, num) in [3u32, 4, 5].into_iter().enumerate() {
        b.obj(num, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 6 0 R >> >> /Contents {} 0 R >>", 10 + i));
        let content = text_at(&[
            ("F1", &format!("Body text of page {}, nothing special", i + 1), 10.0, 72.0, 600.0),
            ("F1", "Quarterly Report", 9.0, 72.0, 30.0),
            ("F1", &format!("{}", i + 1), 9.0, 290.0, 20.0),
        ]);
        b.stream_obj(10 + i as u32, "", content.as_bytes());
    }
    let (docx, _) = convert(b.finish_classic(20, "/Root 1 0 R"));
    let text = docx.text();
    assert!(text.contains("Body text of page 1") && text.contains("Body text of page 3"), "{text}");
    assert!(!text.contains("Quarterly Report"), "{text}");
    let footer = docx.part("word/footer1.xml");
    assert!(footer.contains("Quarterly Report") && footer.contains("PAGE"), "{footer}");
}

#[test]
fn a_picture_is_a_file_in_the_package_and_a_drawing_in_the_text() {
    // A 2 by 2 RGB image drawn 100 points wide.
    let image = |b: &mut PdfBuilder| {
        b.stream_obj(7, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    };
    let content = format!("{}q 100 0 0 100 72 500 cm /Im1 Do Q\n", text_at(&[("F1", "Text before the picture.", 10.0, 72.0, 700.0)]));
    let (docx, report) = convert(page_with(&content, (595, 842), image, "/XObject << /Im1 7 0 R >>"));
    assert_eq!(report.counts.images, 1);
    let png = &docx.parts.iter().find(|(n, _)| n == "word/media/image1.png").expect("the picture").1;
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let doc = docx.document();
    assert!(doc.contains("<w:drawing>") && doc.contains("r:embed=\"rId101\"") && doc.contains("<wp:extent cx=\"1270000\" cy=\"1270000\"/>"), "{doc}");
    assert!(docx.part("word/_rels/document.xml.rels").contains("Id=\"rId101\"") && docx.part("[Content_Types].xml").contains("image/png"));
    assert!(doc.find("Text before").unwrap() < doc.find("<w:drawing>").unwrap());
}

#[test]
fn a_page_of_vertical_text_is_a_vertical_section() {
    let doc = Document::open(common::public_file("cjk/vertical.pdf")).unwrap();
    let mut out = Vec::new();
    docx::export(&doc, "", None, &mut out, None).unwrap();
    let docx = Docx { parts: unzip(&out) };
    assert!(docx.document().contains("<w:textDirection w:val=\"tbRl\"/>"), "{}", docx.document());
    assert!(docx.text().contains("あいうえお"), "{}", docx.text());
}

#[test]
fn the_limits_hold() {
    let doc = Document::from_bytes(page_with("BT /F1 12 Tf 72 700 Td (x) Tj ET", (595, 842), |_| {}, "")).unwrap();
    // A page that is not there.
    assert!(matches!(docx::export(&doc, "", Some(&[5]), Vec::new(), None), Err(Error::Invalid(_))));
    // Too many pages.
    let many: Vec<usize> = vec![0; docx::MAX_PAGES + 1];
    assert!(matches!(docx::export(&doc, "", Some(&many), Vec::new(), None), Err(Error::Limit(_))));
    // Stopped.
    let stop = Arc::new(AtomicBool::new(true));
    assert!(matches!(docx::export(&doc, "", None, Vec::new(), Some(stop)), Err(Error::Cancelled)));
}

#[test]
fn the_damaged_and_hostile_files_of_the_corpus_end_in_a_file_or_an_error() {
    let mut converted = 0;
    for path in common::walk_pdfs(&common::corpus_root().join("public").join("damaged")).into_iter().chain(common::walk_pdfs(&common::corpus_root().join("public").join("encrypted"))) {
        let Ok(doc) = Document::open(&path) else { continue };
        let mut out = Vec::new();
        match docx::export(&doc, "", None, &mut out, None) {
            Ok(r) => {
                converted += 1;
                let parts = unzip(&out);
                for (name, data) in parts.iter().filter(|(n, _)| n.ends_with(".xml")) {
                    well_formed(std::str::from_utf8(data).unwrap()).unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
                }
                assert!(r.pages <= 100_000);
            }
            Err(e) => {
                let _ = e.to_string();
            }
        }
    }
    assert!(converted > 3, "only {converted} converted");
}

#[test]
fn an_encrypted_file_converts_with_its_password() {
    let path = common::public_file("encrypted/encrypted_hello_world_r6.pdf");
    let doc = Document::open_with_password(&path, &common::password_for(&path)).unwrap();
    let mut out = Vec::new();
    docx::export(&doc, &common::password_for(&path), None, &mut out, None).unwrap();
    let docx = Docx { parts: unzip(&out) };
    assert!(docx.text().to_lowercase().contains("hello"), "{}", docx.text());
}

// --- after the review of 5b -----------------------------------------------------------------------------------------------------

/// The numbers of an attribute list such as `<w:pgMar w:top="1" ...>`: `(name, value)` of every `w:name="number"` of the tag.
fn numbers_of(xml: &str, tag: &str) -> Vec<(String, i64)> {
    let at = xml.find(&format!("<{tag} ")).unwrap_or_else(|| panic!("no <{tag}> in {xml}"));
    let end = at + xml[at..].find('>').unwrap();
    xml[at..end]
        .split_whitespace()
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((k.trim_start_matches("w:").to_string(), v.trim_matches(|c| c == '"' || c == '/').parse().ok()?))
        })
        .collect()
}

fn number(list: &[(String, i64)], name: &str) -> i64 {
    list.iter().find(|(k, _)| k == name).unwrap_or_else(|| panic!("no {name} in {list:?}")).1
}

#[test]
fn a_page_of_four_thousand_crossing_rules_is_no_trouble() {
    // 1990 rules across and 1990 down, every one crossing all of the other direction: a hatched drawing, no table. Joining them
    // one pair at a time took seconds a page (the time grew with the cube of the number of rules).
    let mut c = String::from("0.5 w\n");
    for i in 0..1990 {
        let y = 10 + 3 * i;
        c.push_str(&format!("10 {y} m 6000 {y} l S\n{y} 10 m {y} 6000 l S\n"));
    }
    c.push_str(&text_at(&[("F1", "Text on the page with the grid.", 12.0, 100.0, 300.0)]));
    let started = std::time::Instant::now();
    let (docx, report) = convert(page_with(&c, (6100, 6100), |_| {}, ""));
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
    assert_eq!(report.counts.tables, 0);
    assert!(docx.text().contains("Text on the page with the grid."), "{}", docx.text());
}

#[test]
fn a_page_bigger_than_words_biggest_is_made_smaller_as_a_whole() {
    // 36 by 48 inches, with a heading, a picture of 1000 points and text: Word's pages are 22 inches at most.
    let image = |b: &mut PdfBuilder| {
        b.stream_obj(7, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    };
    let content = format!(
        "{}q 1000 0 0 1000 500 1000 cm /Im1 Do Q\n",
        text_at(&[("F2", "A poster heading", 96.0, 200.0, 3200.0), ("F1", "Some text of the poster at the usual size of a poster.", 48.0, 200.0, 2800.0)])
    );
    let (docx, report) = convert(page_with(&content, (2592, 3456), image, "/XObject << /Im1 7 0 R >>"));
    let doc = docx.document();
    let size = numbers_of(doc, "w:pgSz");
    let (w, h) = (number(&size, "w"), number(&size, "h"));
    // 22 inches is 31680 twips; the shape of the page is the same (3 : 4).
    assert_eq!((w, h), (23_760, 31_680), "{doc}");
    let margins = numbers_of(doc, "w:pgMar");
    for (side, dim) in [("top", h), ("bottom", h), ("left", w), ("right", w)] {
        assert!((0..dim / 2).contains(&number(&margins, side)), "{side}: {margins:?}");
    }
    // The picture is 1000 points on the big page, 458 on the small one (k = 22 * 72 / 3456); the text is smaller by the same.
    let extent = numbers_of(doc, "wp:extent");
    let (cx, cy) = (number(&extent, "cx"), number(&extent, "cy"));
    let wanted = (1000.0 * 1584.0 / 3456.0 * 12_700.0) as i64;
    assert!(cx == cy && (cx - wanted).abs() < wanted / 100, "{cx} {cy} {wanted}");
    assert!(docx.text().contains("A poster heading") && docx.text().contains("Some text of the poster"), "{}", docx.text());
    assert_eq!(report.counts.images, 1);
    // The size of the text: 48 points on the big page are 22 on the small one (the usual size, in the styles).
    assert!(docx.part("word/styles.xml").contains("<w:sz w:val=\"44\"/>"), "{}", docx.part("word/styles.xml"));
    for (name, data) in &docx.parts {
        if name.ends_with(".xml") {
            assert!(!std::str::from_utf8(data).unwrap().contains("w:sz w:val=\"0\""), "{name}");
        }
    }
}

#[test]
fn a_page_of_an_absurd_size_is_a_page_that_word_opens() {
    let content = format!("{}q 100 0 0 100 100 300 cm /Im1 Do Q\n", text_at(&[("F1", "Hello world hello", 12.0, 72.0, 700.0), ("F1", "A second line of the text.", 12.0, 72.0, 686.0)]));
    let huge = format!("1{}", "0".repeat(30));
    let pdf = {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {huge} {huge}] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 7 0 R >> >> /Contents 4 0 R >>"));
        b.stream_obj(4, "", content.as_bytes());
        b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
        b.stream_obj(7, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 85, 170, 255]);
        b.finish_classic(20, "/Root 1 0 R")
    };
    let (docx, _) = convert(pdf);
    let doc = docx.document();
    let size = numbers_of(doc, "w:pgSz");
    let (w, h) = (number(&size, "w"), number(&size, "h"));
    assert!((1_440..=31_680).contains(&w) && (1_440..=31_680).contains(&h), "{size:?}");
    let margins = numbers_of(doc, "w:pgMar");
    for (side, dim) in [("top", h), ("bottom", h), ("left", w), ("right", w)] {
        assert!((0..dim / 2).contains(&number(&margins, side)), "{side}: {margins:?}");
    }
    if doc.contains("<wp:extent") {
        let extent = numbers_of(doc, "wp:extent");
        assert!((1..=1584 * 12_700).contains(&number(&extent, "cx")) && (1..=1584 * 12_700).contains(&number(&extent, "cy")), "{extent:?}");
    }
    assert!(docx.text().contains("Hello world hello"), "{}", docx.text());
    // The text on such a page has no size that means anything: the usual size of a Chinese document's text is written, never 0.
    let styles = docx.part("word/styles.xml");
    assert!(styles.contains("<w:sz w:val=\"21\"/>"), "{styles}");
    for (name, data) in &docx.parts {
        if name.ends_with(".xml") {
            assert!(!std::str::from_utf8(data).unwrap().contains("w:sz w:val=\"0\""), "{name}");
        }
    }
}

#[test]
fn every_repeated_line_of_the_page_header_and_footer_is_kept() {
    // Three pages, each with two lines at the top, two lines at the bottom and a page number; and one more line at the top of the
    // second page only, which stays in the text.
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
    b.obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for (i, num) in [3u32, 4, 5].into_iter().enumerate() {
        b.obj(num, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 6 0 R >> >> /Contents {} 0 R >>", 10 + i));
        let page = i + 1;
        let body = format!("Body text of page {page}, nothing special");
        let number = page.to_string();
        let mut lines = vec![
            ("F1", "Journal of Computer Tests", 9.0, 72.0, 820.0),
            ("F1", "http://www.example.cn", 9.0, 72.0, 775.0),
            ("F1", body.as_str(), 10.0, 72.0, 600.0),
            ("F1", "Second line of the footer", 9.0, 72.0, 45.0),
            ("F1", "Quarterly Report", 9.0, 72.0, 25.0),
            ("F1", number.as_str(), 9.0, 290.0, 12.0),
        ];
        if page == 2 {
            lines.push(("F1", "Only on the second page", 9.0, 300.0, 800.0));
        }
        b.stream_obj(10 + i as u32, "", text_at(&lines).as_bytes());
    }
    let (docx, _) = convert(b.finish_classic(20, "/Root 1 0 R"));
    let text = docx.text();
    let (header, footer) = (docx.part("word/header1.xml"), docx.part("word/footer1.xml"));
    for line in ["Journal of Computer Tests", "http://www.example.cn"] {
        assert!(header.contains(line) && !text.contains(line), "{line}: {header}\n{text}");
    }
    for line in ["Second line of the footer", "Quarterly Report"] {
        assert!(footer.contains(line) && !text.contains(line), "{line}: {footer}\n{text}");
    }
    assert!(footer.contains("PAGE") && !header.contains("PAGE"), "{footer}");
    // Each line once, and a line that is not on every page is where it was.
    assert_eq!(header.matches("Journal of Computer Tests").count(), 1);
    assert!(text.contains("Only on the second page") && !header.contains("Only on the second page"), "{text}");
    for page in 1..=3 {
        assert!(text.contains(&format!("Body text of page {page}")));
    }
}

/// A PDF of `pages` pages, each with a line of text and a gray picture of `side` by `side` pixels (all zero, so that it is small in
/// the file and big once decoded; every page has an image object of its own).
fn scan_pages(pages: usize, side: usize) -> Vec<u8> {
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; side * side], 1);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 100 + 3 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for i in 0..pages {
        let n = 100 + 3 * i as u32;
        b.obj(n, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 {} 0 R >> >> /Contents {} 0 R >>", n + 2, n + 1));
        let content = format!("BT /F1 12 Tf 72 700 Td (Scanned page number {i} has its text.) Tj ET\nq 300 0 0 300 100 200 cm /Im1 Do Q\n");
        b.stream_obj(n + 1, "", content.as_bytes());
        b.stream_obj(n + 2, &format!("/Type /XObject /Subtype /Image /Width {side} /Height {side} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"), &packed);
    }
    b.finish_classic(100 + 3 * pages as u32, "/Root 1 0 R")
}

#[test]
fn the_pictures_of_a_long_scan_do_not_use_up_the_decoding_allowance_of_the_text() {
    // 30 pages of 6900 by 6900 pixels (47 MB each once decoded, the most a picture may be): more than the 1 GiB that a whole file may decode. Every page has
    // an allowance of its own, as it has when it is drawn; so all the pages are converted, with their pictures.
    let doc = Document::from_bytes(scan_pages(30, 6_900)).unwrap();
    let mut out = Vec::new();
    let report = docx::export(&doc, "", None, &mut out, None).unwrap();
    let docx = Docx { parts: unzip(&out) };
    let text = docx.text();
    for page in 0..30 {
        assert!(text.contains(&format!("Scanned page number {page} has its text.")), "page {page} lost its text: {report:?}\n{text}");
    }
    assert_eq!(report.pages, 30, "{:?}", report.warnings);
    assert_eq!(report.counts.images, 30, "{:?}", report.warnings);
}

#[test]
fn a_picture_that_is_too_much_is_left_out_and_the_text_of_the_page_stays() {
    // One picture of 12000 by 12000 pixels is more than a picture may be (128 million pixels); the page still has its text, and
    // the picture of the page after it, which is fine, is there.
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 1000], 1);
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for (i, num) in [3u32, 4].into_iter().enumerate() {
        b.obj(num, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 {} 0 R >> >> /Contents {} 0 R >>", 20 + i, 10 + i));
        let content = format!("BT /F1 12 Tf 72 700 Td (Text of page {} that must stay.) Tj ET\nq 200 0 0 200 100 300 cm /Im1 Do Q\n", i + 1);
        b.stream_obj(10 + i as u32, "", content.as_bytes());
    }
    b.stream_obj(20, "/Type /XObject /Subtype /Image /Width 12000 /Height 12000 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode", &packed);
    b.stream_obj(21, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 85, 170, 255]);
    let (docx, report) = convert(b.finish_classic(30, "/Root 1 0 R"));
    assert!(docx.text().contains("Text of page 1 that must stay.") && docx.text().contains("Text of page 2 that must stay."), "{}", docx.text());
    assert_eq!((report.pages, report.counts.images), (2, 1), "{:?}", report.warnings);
    assert!(report.warnings.iter().any(|w| w.contains("page 1") && w.contains("picture")), "{:?}", report.warnings);
}
