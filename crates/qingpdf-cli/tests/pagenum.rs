//! `qingpdf pagenum` (page numbers, headers and footers), run as a user would: arguments in, files and exit codes out. What it writes is
//! checked the way the engine's tests check it (the original bytes are a prefix, `qpdf --check`, the text of the pages is as it was); where
//! the numbers are on the page is checked by the engine's tests and by `tests/tools/pagenum_check.py` (PDFium).
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};
use std::time::Instant;

use runner::{code, run, s, stderr, stdout};

fn has_sans_font() -> bool {
    ["arial.ttf", "msyh.ttc", "simsun.ttc"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file())
}

/// A file of `pages` pages of 300 by 400 points that share one `/Resources` (a font) and inherit their `/MediaBox`; each page has a
/// content stream of its own.
fn many_pages(pages: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let mut put = |out: &mut Vec<u8>, n: u32, body: String| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    put(&mut out, 1, "<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    put(&mut out, 2, format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /Resources 3 0 R /MediaBox [0 0 300 400] >>"));
    put(&mut out, 3, "<< /Font << /Helv 4 0 R >> >>".to_string());
    put(&mut out, 4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
    for i in 0..pages {
        let text = format!("BT /Helv 14 Tf 30 350 Td (page {}) Tj ET", i + 1);
        put(&mut out, 10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i));
        put(&mut out, 11 + 2 * i, format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    }
    let size = 10 + 2 * pages;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None if n == 0 => out.extend_from_slice(b"0000000000 65535 f \n"),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

fn check_written(input: &Path, output: &Path, password: &str) {
    let before = std::fs::read(input).unwrap();
    let after = std::fs::read(output).unwrap();
    assert!(after.len() > before.len() && after.starts_with(&before), "the bytes of the input are not a prefix of the output");
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = if password.is_empty() { common::qpdf_check(&qpdf, output) } else { common::qpdf_check_pw(&qpdf, output, password) };
        assert!(verdict.code == 0, "qpdf --check says {}:\n{}", verdict.code, verdict.text);
    }
}

/// The pages (from 1) that have page numbers of ours.
fn numbered(file: &Path, password: &str) -> Vec<usize> {
    let doc = qingpdf_core::Document::open_with_password(file, password).unwrap();
    let pages = doc.pages().unwrap();
    (0..pages.len()).filter(|&i| matches!(doc.get(pages[i].obj_ref), Ok(qingpdf_core::Object::Dict(d)) if d.get("QpPagination").is_some())).map(|i| i + 1).collect()
}

fn setup(name: &str, pages: u32) -> (PathBuf, PathBuf) {
    let dir = common::fresh_out_dir(name);
    let input = dir.join("in.pdf");
    std::fs::write(&input, many_pages(pages)).unwrap();
    (dir, input)
}

#[test]
fn pagenum_has_help_and_wrong_command_lines_exit_with_2() {
    assert!(stdout(&run(["--help"])).contains("pagenum"));
    let help = stdout(&run(["pagenum", "--help"]));
    assert!(help.lines().any(|l| l.contains("qingpdf pagenum")));
    for word in ["--header-left", "--footer-right", "--size", "--color", "--margin", "--pages", "--start", "--mirror", "--skip-first", "{page}", "{pages}", "{date}", "{file}"] {
        assert!(help.contains(word), "the help does not say {word}");
    }
    let (dir, input) = setup("cli-pagenum-usage", 3);
    let (a, o) = (s(&input).to_string(), s(&dir.join("o.pdf")).to_string());
    for args in [
        vec!["pagenum", &a],
        vec!["pagenum", &a, "--footer-center", "x"],
        vec!["pagenum", &a, "-o", &o, "--color", "red"],
        vec!["pagenum", &a, "-o", &o, "--color", "12345"],
        vec!["pagenum", &a, "-o", &o, "--margin", "-1"],
        vec!["pagenum", &a, "-o", &o, "--margin", "501"],
        vec!["pagenum", &a, "-o", &o, "--margin", "abc"],
        vec!["pagenum", &a, "-o", &o, "--start", "-1"],
        vec!["pagenum", &a, "-o", &o, "--start", "1.5"],
        vec!["pagenum", &a, "-o", &o, "--start", "1000001"],
        vec!["pagenum", &a, "-o", &o, "--size", "0"],
        vec!["pagenum", &a, "-o", &o, "--size", "big"],
        vec!["pagenum", &a, "-o", &o, "--mirror=yes"],
        vec!["pagenum", &a, "-o", &o, "--skip-first=1"],
        vec!["pagenum", &a, "-o", &o, "--footer-left", "a", "--footer-left", "b"],
        vec!["pagenum", &a, "-o", &o, "--color", "000000", "--color", "ffffff"],
        vec!["pagenum", &a, "-o", &o, "--footer-left", " ", "--header-right", ""],
        vec!["pagenum", &a, "-o", &o, "--text", "x"],
        vec!["pagenum", &a, "-o", &o, "--header-left"],
        vec!["watermark", &a, "--text", "x", "--margin", "5", "-o", &o],
        vec!["rotate", &a, "--angle", "90", "--start", "1", "-o", &o],
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("Try 'qingpdf"), "{args:?}");
    }
    assert!(!dir.join("o.pdf").exists());
}

#[test]
fn with_no_place_named_the_number_is_in_the_middle_of_the_foot_and_the_text_is_as_it_was() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let (dir, input) = setup("cli-pagenum-default", 3);
    let output = dir.join("out.pdf");
    let out = run(["pagenum", s(&input), "-o", s(&output)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("wrote") && stderr(&out).is_empty(), "{} {}", stdout(&out), stderr(&out));
    check_written(&input, &output, "");
    assert_eq!(numbered(&output, ""), vec![1, 2, 3]);
    // Artifacts: the text of every page is what it was.
    let (before, after) = (run(["text", s(&input)]), run(["text", s(&output)]));
    assert_eq!((code(&before), code(&after)), (0, 0));
    assert_eq!(stdout(&before), stdout(&after));
    assert!(stdout(&after).contains("page 3"));
    // The output may not be the input, and an output that is there is not overwritten without --force.
    assert_eq!(code(&run(["pagenum", s(&input), "-o", s(&input), "--force"])), 1);
    let again = run(["pagenum", s(&input), "-o", s(&output)]);
    assert_eq!(code(&again), 1);
    assert!(stderr(&again).contains("--force"));
}

#[test]
fn the_options_pages_start_mirror_skip_first_colour_size_and_margin_are_taken() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let (dir, input) = setup("cli-pagenum-options", 6);
    let output = dir.join("out.pdf");
    let out = run([
        "pagenum", s(&input), "--header-left", "{file}", "--header-center", "{date}", "--footer-right", "Page {page} of {pages}", "--footer-left", "{page}", "--size", "9.5", "--color", "#1f3a93", "--margin", "12.5", "--pages", "3-5",
        "--start", "7", "--mirror", "--skip-first", "-o", s(&output),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    check_written(&input, &output, "");
    // Pages 3 to 5, the first left out: pages 4 and 5 only.
    assert_eq!(numbered(&output, ""), vec![4, 5]);
    // A size outside what text may be is changed to the nearest one, with a warning; a place with only blanks and none at all are refused.
    let out = run(["pagenum", s(&input), "--size", "1000", "-o", s(&output), "--force"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stderr(&out).contains("warning:") && stderr(&out).contains("size 1000 was changed to 144"), "{}", stderr(&out));
    // A page list that names pages the file does not have, and a list of one page left out.
    let out = run(["pagenum", s(&input), "--pages", "7", "-o", s(&output), "--force"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let out = run(["pagenum", s(&input), "--pages", "2", "--skip-first", "-o", s(&output), "--force"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("left out"), "{}", stderr(&out));
    // A second run on the output adds a second set (the pages have two streams of ours).
    let first = dir.join("first.pdf");
    assert_eq!(code(&run(["pagenum", s(&input), "-o", s(&first)])), 0);
    let second = dir.join("second.pdf");
    assert_eq!(code(&run(["pagenum", s(&first), "--header-right", "again", "-o", s(&second)])), 0);
    check_written(&first, &second, "");
    let doc = qingpdf_core::Document::open(&second).unwrap();
    let page = &doc.pages().unwrap()[0];
    let Ok(qingpdf_core::Object::Dict(d)) = doc.get(page.obj_ref) else { panic!() };
    assert!(matches!(d.get("QpPagination"), Some(qingpdf_core::Object::Array(a)) if a.len() == 2));
}

#[test]
fn a_signed_file_and_a_file_that_may_not_be_changed_are_refused_and_an_encrypted_one_keeps_its_encryption() {
    let dir = common::fresh_out_dir("cli-pagenum-rules");
    let input = dir.join("in.pdf");
    std::fs::write(&input, many_pages(3)).unwrap();
    let output = dir.join("out.pdf");
    // A signed file.
    let signed = "%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] /SigFlags 3 >> >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Annots [10 0 R] >>\nendobj\n10 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V 20 0 R >>\nendobj\n20 0 obj\n<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n";
    let signed_path = dir.join("signed.pdf");
    std::fs::write(&signed_path, signed).unwrap();
    let out = run(["pagenum", s(&signed_path), "-o", s(&output)]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("signed") || stderr(&out).contains("signature"), "{}", stderr(&out));
    assert!(!output.exists());
    let Some(qpdf) = common::find_qpdf() else { return };
    // Content may not be changed (permission bit 4): refused, and the owner's password lifts it.
    let locked = dir.join("locked.pdf");
    let made = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner", "256", "--modify=none", "--", s(&input), s(&locked)]).status().unwrap();
    assert!(made.code().is_some_and(|c| c == 0 || c == 3));
    let out = run(["pagenum", s(&locked), "-o", s(&output)]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("permission bit 4") && stderr(&out).contains("owner password"), "{}", stderr(&out));
    if has_sans_font() {
        let out = run(["pagenum", s(&locked), "--password", "owner", "-o", s(&output)]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        check_written(&locked, &output, "owner");
        assert_eq!(numbered(&output, "owner"), vec![1, 2, 3]);
        // The result is still encrypted: it does not open without the password the file asks for (the owner's), but opens with it.
        assert!(qingpdf_core::Document::open_with_password(&output, "owner").unwrap().is_encrypted());
    }
}

#[test]
fn numbers_on_500_pages_take_under_two_seconds_and_make_one_font_for_all_of_them() {
    if !has_sans_font() {
        println!("SKIPPED: no TrueType font on this machine");
        return;
    }
    let (dir, input) = setup("cli-pagenum-500", 500);
    let output = dir.join("out.pdf");
    let started = Instant::now();
    let out = run(["pagenum", s(&input), "--header-left", "{file}", "--header-right", "{date}", "--footer-center", "{page} / {pages}", "-o", s(&output)]);
    let took = started.elapsed();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    println!("500 pages in {took:?}");
    if !cfg!(debug_assertions) {
        assert!(took.as_secs_f64() <= 2.0, "500 pages took {took:?}");
    }
    check_written(&input, &output, "");
    let section = &std::fs::read(&output).unwrap()[std::fs::read(&input).unwrap().len()..];
    let text = String::from_utf8_lossy(section);
    assert_eq!(text.matches("/Type /FontDescriptor").count(), 1, "the font was made again for pages");
    assert_eq!(text.matches("/QpPagination").count(), 500);
    // The pages share their resources: one copy of them for all (not one for each page).
    assert!(text.matches("/Font <<").count() <= 2, "the resources were copied for each page");
    assert_eq!(numbered(&output, "").len(), 500);
}
