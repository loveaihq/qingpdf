//! `qingpdf redact` (item 8), run as a user would: arguments in, files and exit codes out. What it writes is looked at the way someone who wanted the
//! content back would look (our own text extraction, every stream of the file decoded, `qpdf --check` where qpdf is there), and the speed on a
//! file of a hundred pages is measured. The other readers are checked by `tests/tools/redact_check.py`.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use qingpdf_core::Document;
use qingpdf_core::object::{ObjRef, Object};
use runner::{code, run, s, stderr, stdout};

/// The widths of Helvetica for the characters 32 to 126, in thousandths.
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278,
    500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 222, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500,
    278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

fn out_dir(name: &str) -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("redact-cli").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file of `pages` pages of 300 by 400 points; the page `N` has `secret N` (kerned: `sec` and `ret` are two pieces) and the same line every page.
fn many_pages(pages: u32, extra_catalog: &str, extra: &str, first_tail: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let mut put = |out: &mut Vec<u8>, n: u32, body: String| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    put(&mut out, 1, format!("<< /Type /Catalog /Pages 2 0 R {extra_catalog} >>"));
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    put(&mut out, 2, format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /Resources 3 0 R /MediaBox [0 0 300 400] >>"));
    put(&mut out, 3, "<< /Font << /Helv 4 0 R >> >>".to_string());
    // (Helvetica with its widths: a font without them has its letters put where a guess says, and the whole line goes.)
    let widths: Vec<String> = HELV.iter().map(u16::to_string).collect();
    put(&mut out, 4, format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{}] >>", widths.join(" ")));
    for i in 0..pages {
        let tail = if i == 0 && !first_tail.is_empty() { first_tail.to_string() } else { (i + 1).to_string() };
        let text = format!("BT /Helv 14 Tf 30 350 Td [(sec) 20 (ret)] TJ ( {tail}) Tj 0 -16 Td [(The) -250 (quick) -250 (brown) -250 (fox)] TJ ET");
        put(&mut out, 10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i));
        put(&mut out, 11 + 2 * i, format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    }
    put(&mut out, 5, "<< /Title (Secret Title) /Author (Someone) >>".to_string());
    for (n, body) in extra.split(";;").filter(|b| !b.trim().is_empty()).enumerate() {
        put(&mut out, 6 + n as u32, body.trim().to_string());
    }
    let size = 10 + 2 * pages;
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some(at) => out.extend_from_slice(format!("{at:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
    out
}

fn everything(path: &Path) -> Vec<u8> {
    let doc = Document::from_bytes(std::fs::read(path).unwrap()).unwrap();
    let mut all = Vec::new();
    for n in 1..doc.next_free_number() {
        match doc.get(ObjRef::new(n, 0)) {
            Ok(Object::Stream(s)) => {
                qingpdf_core::writer::write_object(&mut all, &Object::Dict(s.dict.clone())).unwrap();
                all.extend(doc.decode_stream(&s).unwrap_or_default());
            }
            Ok(o) => qingpdf_core::writer::write_object(&mut all, &o).unwrap(),
            Err(_) => {}
        }
    }
    all
}

fn has(haystack: &[u8], needle: &str) -> bool {
    common::contains(haystack, needle.as_bytes())
}

#[test]
fn redact_takes_the_text_out_says_what_it_did_and_the_output_is_a_whole_new_file() {
    let dir = out_dir("basic");
    let input = dir.join("in.pdf");
    let original = many_pages(5, "", "", "");
    std::fs::write(&input, &original).unwrap();
    let output = dir.join("out.pdf");
    let r = run(["redact", s(&input), "--find", "secret", "--find", "quick brown", "--label", "X", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let said = stdout(&r);
    assert!(said.contains("page 1: 16 characters"), "{said}");
    assert!(said.contains("redacted 10 places on 5 pages"), "{said}");
    let written = std::fs::read(&output).unwrap();
    // Written afresh: not the old file with an update.
    assert!(!written.starts_with(&original));
    let all = everything(&output);
    for gone in ["secret", "sec", "quick", "brown"] {
        assert!(!has(&all, gone), "{gone} is still in the file");
    }
    assert!(has(&all, "The") && has(&all, "fox"));
    let text = stdout(&run(["text", s(&output)]));
    assert!(!text.contains("secret") && !text.contains("quick") && text.contains("The") && text.contains("fox") && text.contains("5"), "{text}");
    if let Some(qpdf) = common::find_qpdf() {
        let check = Command::new(qpdf).arg("--check").arg(&output).output().unwrap();
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stdout));
    }
    // Nothing found: exit status 1 and no file; a word that is found nowhere is said when others are found.
    let missing = dir.join("none.pdf");
    let r = run(["redact", s(&input), "--find", "absent", "-o", s(&missing)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("nothing to redact") && !missing.exists(), "{}", stderr(&r));
    let some = dir.join("some.pdf");
    let r = run(["redact", s(&input), "--find", "secret", "--find", "absent", "-o", s(&some), "--force"]);
    assert_eq!(code(&r), 0);
    assert!(stderr(&r).contains("'absent' was not found"), "{}", stderr(&r));
    // The output is not the input; an existing output needs --force.
    assert_eq!(code(&run(["redact", s(&input), "--find", "secret", "-o", s(&input), "--force"])), 1);
    assert_eq!(code(&run(["redact", s(&input), "--find", "secret", "-o", s(&output)])), 1);
    // Not given what is needed, or given wrongly: 2.
    for args in [
        vec!["redact", s(&input), "-o", s(&missing)],
        vec!["redact", s(&input), "--find", "a"],
        vec!["redact", s(&input), "--find", "", "-o", s(&missing)],
        vec!["redact", s(&input), "--pattern", "passport", "-o", s(&missing)],
        vec!["redact", s(&input), "--area", "1:1,2,3", "-o", s(&missing)],
        vec!["redact", s(&input), "--area", "0:1,2,3,4", "-o", s(&missing)],
        vec!["redact", s(&input), "--find", "a", "--color", "red", "-o", s(&missing)],
        vec!["redact", s(&input), "--find", "a", "--pages", "1", "-o", s(&missing)],
    ] {
        let r = run(&args);
        assert_eq!(code(&r), 2, "{args:?}: {}", stderr(&r));
    }
    // A page that is not there.
    assert_eq!(code(&run(["redact", s(&input), "--area", "9:10,10,20,20", "-o", s(&missing)])), 1);
}

#[test]
fn an_area_a_kind_of_number_and_the_metadata() {
    let dir = out_dir("areas");
    let input = dir.join("in.pdf");
    // Page 1 has a phone number; the information dictionary has a title.
    std::fs::write(&input, many_pages(2, "", "", "13812345678")).unwrap();
    let output = dir.join("out.pdf");
    // The text of page 2 is hidden by an area (the line "The quick brown fox" is at 350 - 16 = 334 from the bottom, 14 points high).
    let r = run(["redact", s(&input), "--pattern", "phone", "--area", "2:25,328,200,20", "--color", "ff0000", "--clear-metadata", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let all = everything(&output);
    assert!(!has(&all, "13812345678") && !has(&all, "Secret Title") && !has(&all, "Someone"), "{}", String::from_utf8_lossy(&all));
    let text = stdout(&run(["text", s(&output), "--pages", "2"]));
    assert!(!text.contains("quick") && text.contains("secret"), "{text}");
    // Without --clear-metadata the information is kept.
    let kept = dir.join("kept.pdf");
    assert_eq!(code(&run(["redact", s(&input), "--pattern", "phone", "-o", s(&kept), "--force"])), 0);
    assert!(has(&everything(&kept), "Secret Title"));
    // A red box was drawn.
    let content = everything(&output);
    assert!(has(&content, "1.0000 0.0000 0.0000 rg"), "{}", String::from_utf8_lossy(&content));
}

#[test]
fn redact_keeps_the_encryption_and_wants_the_right_to_change_the_content() {
    let dir = out_dir("encrypted");
    let input = dir.join("in.pdf");
    std::fs::write(&input, many_pages(2, "", "", "")).unwrap();
    let locked = dir.join("locked.pdf");
    let r = run(["encrypt", s(&input), "--owner-password", "ownerpw", "--no-modify", "-o", s(&locked), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    // Without the owner's password the content may not be changed.
    let output = dir.join("out.pdf");
    let _ = std::fs::remove_file(&output);
    let r = run(["redact", s(&locked), "--find", "secret", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("did not allow changing its content") && !output.exists(), "{}", stderr(&r));
    // With it, the file is redacted and is still encrypted (opens without a password because the user password is empty; the permission stays).
    let r = run(["redact", s(&locked), "--find", "secret", "--password", "ownerpw", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let info = stdout(&run(["info", s(&output)]));
    assert!(info.contains("Encrypted:") && info.contains("yes"), "{info}");
    let text = stdout(&run(["text", s(&output), "--password", "ownerpw"]));
    assert!(!text.contains("secret") && text.contains("quick"), "{text}");
    // A signed file is refused.
    let signed = dir.join("signed.pdf");
    std::fs::write(&signed, many_pages(1, "/AcroForm << /Fields [6 0 R] >>", "<< /FT /Sig /T (s) /V 7 0 R >> ;; << /Type /Sig /Filter /Adobe.PPKLite >>", "")).unwrap();
    let r = run(["redact", s(&signed), "--find", "secret", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("signed"), "{}", stderr(&r));
}

#[test]
fn twenty_places_in_a_hundred_pages_take_a_moment() {
    let dir = out_dir("speed");
    let input = dir.join("in.pdf");
    // Twenty pages have the word; the other eighty do not (the word is cut from them by replacing it).
    let text = String::from_utf8_lossy(&many_pages(100, "", "", "")).into_owned();
    // Keep the word on pages 5, 10, ..., 100 only: `[(sec) 20 (ret)]` on the others becomes `[(sex) 20 (ret)]` (as long: the offsets stay true).
    let mut out = String::new();
    for (page, part) in text.split("[(sec) 20 (ret)]").enumerate() {
        if page > 0 {
            out.push_str(if page % 5 == 0 { "[(sec) 20 (ret)]" } else { "[(sex) 20 (ret)]" });
        }
        out.push_str(part);
    }
    std::fs::write(&input, out.as_bytes()).unwrap();
    let output = dir.join("out.pdf");
    let started = Instant::now();
    let r = run(["redact", s(&input), "--find", "secret", "-o", s(&output), "--force"]);
    let took = started.elapsed();
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("redacted 20 places on 20 pages"), "{}", stdout(&r));
    println!("20 places in 100 pages: {took:?}");
    assert!(took.as_secs_f64() <= 2.0, "{took:?}");
}

#[test]
fn attachments_and_scripts_are_counted_and_the_option_takes_them_out() {
    let dir = out_dir("active");
    let input = dir.join("in.pdf");
    let file = "the file says secret too";
    let extra = format!(
        "<< /Names [(a.txt) 7 0 R] >> ;; << /Type /Filespec /F (a.txt) /EF << /F 8 0 R >> >> ;; << /Type /EmbeddedFile /Length {} >>\nstream\n{file}\nendstream ;; << /S /JavaScript /JS (app.alert\\('secret'\\)) >>",
        file.len()
    );
    std::fs::write(&input, many_pages(2, "/Names << /EmbeddedFiles 6 0 R /JavaScript << /Names [(init) 9 0 R] >> >>", &extra, "")).unwrap();
    let output = dir.join("out.pdf");
    let _ = std::fs::remove_file(&output);
    let r = run(["redact", s(&input), "--find", "secret", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stderr(&r).contains("1 attachment(s) and 1 script(s)"), "{}", stderr(&r));
    let all = everything(&output);
    assert!(all.windows(12).any(|w| w == b"the file say") && all.windows(9).any(|w| w == b"app.alert"));
    // Asked: gone.
    let _ = std::fs::remove_file(&output);
    let r = run(["redact", s(&input), "--find", "secret", "--remove-attachments", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stderr(&r).contains("Took out 1 attachment(s) and 1 script(s)"), "{}", stderr(&r));
    let all = everything(&output);
    assert!(!all.windows(12).any(|w| w == b"the file say") && !all.windows(9).any(|w| w == b"app.alert"));
}
