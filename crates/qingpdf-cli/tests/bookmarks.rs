//! `qingpdf bookmarks` (2026-10-20), run as a user would: arguments in, files and exit codes out. The text of the bookmarks is written and read
//! again, a file with a bad line is refused with the number of the line, a file is made from its headings, and what is written is checked as
//! the engine's tests check it (the original bytes are a prefix, `qpdf --check`, our reader reads the bookmarks the text says); the other readers
//! are asked by `tests/tools/bookmarks_check.py` (PDFium and PyMuPDF) for the files the engine's tests leave.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};
use std::time::Instant;

use runner::{code, run, s, stderr, stdout};

fn out_dir(name: &str) -> PathBuf {
    common::fresh_out_dir(&format!("cli-bookmarks-{name}"))
}

fn gongwen() -> PathBuf {
    common::public_file("zh/gongwen/gongwen-2025-service-mfg-plan.pdf")
}

/// The bookmarks of `pdf` as the text of `--export` has them.
fn exported(pdf: &Path, dir: &Path, password: &str) -> String {
    let txt = dir.join("exported.txt");
    let mut args = vec!["bookmarks", s(pdf), "--export", s(&txt), "--force"];
    if !password.is_empty() {
        args.extend(["--password", password]);
    }
    let out = run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    std::fs::read_to_string(&txt).unwrap()
}

fn checked(input: &Path, output: &Path, password: &str) {
    let (before, after) = (std::fs::read(input).unwrap(), std::fs::read(output).unwrap());
    assert!(after.len() > before.len() && after.starts_with(&before), "the bytes of the input are not a prefix of the output");
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = if password.is_empty() { common::qpdf_check(&qpdf, output) } else { common::qpdf_check_pw(&qpdf, output, password) };
        assert!(verdict.code == 0, "qpdf --check says {}:\n{}", verdict.code, verdict.text);
    }
}

#[test]
fn help_names_the_command_its_three_forms_and_the_text_format() {
    let text = stdout(&run(["--help"]));
    assert!(text.contains("bookmarks"), "{text}");
    let help = stdout(&run(["bookmarks", "--help"]));
    for word in ["--export", "--import", "--auto", "tab", "page number", "BOM", "byte order mark", "4 MB", "300", "32 levels", "(untitled)", "spaces", "tab inside"] {
        assert!(help.contains(word), "the help of bookmarks should say {word}:\n{help}");
    }
    assert_eq!(code(&run(["help", "bookmarks"])), 0);
}

#[test]
fn the_command_line_is_checked() {
    let dir = out_dir("usage");
    let (pdf, txt, out) = (gongwen(), dir.join("a.txt"), dir.join("o.pdf"));
    let usage = |args: &[&str], why: &str| {
        let r = run(args);
        assert_eq!(code(&r), 2, "{args:?}");
        assert!(stderr(&r).contains(why), "{args:?}: {}", stderr(&r));
    };
    usage(&["bookmarks", s(&pdf)], "--export");
    usage(&["bookmarks", s(&pdf), "--export", s(&txt), "--auto"], "only one of");
    usage(&["bookmarks", s(&pdf), "--import", s(&txt), "--export", s(&txt)], "only one of");
    usage(&["bookmarks", s(&pdf), "--import", s(&txt)], "-o");
    usage(&["bookmarks", s(&pdf), "--auto"], "-o");
    usage(&["bookmarks", s(&pdf), "--export", s(&txt), "-o", s(&out)], "-o is for --import and --auto");
    usage(&["bookmarks", s(&pdf), "--export", s(&txt), "--export", s(&txt)], "more than once");
    usage(&["bookmarks", s(&pdf), "--auto=1", "-o", s(&out)], "does not take a value");
    usage(&["bookmarks", "--export", s(&txt)], "no input file");
    usage(&["bookmarks", s(&pdf), s(&pdf), "--auto", "-o", s(&out)], "one input file");
    usage(&["bookmarks", s(&pdf), "--pages", "1", "--auto", "-o", s(&out)], "unknown option");
}

#[test]
fn export_then_import_gives_the_same_bookmarks() {
    let dir = out_dir("roundtrip");
    let pdf = gongwen();
    let text = exported(&pdf, &dir, "");
    assert!(text.lines().count() >= 15 && text.contains('\t') && text.contains("二、主要任务\t2"), "{text}");
    // A second level has a tab in front; every line has the page after a tab.
    assert!(text.lines().any(|l| l.starts_with('\t') && !l.starts_with("\t\t")), "{text}");
    assert!(text.lines().all(|l| l.rsplit('\t').next().is_some_and(|p| p.is_empty() || p.parse::<u32>().is_ok())));
    let out = dir.join("imported.pdf");
    let r = run(["bookmarks", s(&pdf), "--import", s(&dir.join("exported.txt")), "-o", s(&out)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("bookmarks") && stdout(&r).contains("replacing"), "{}", stdout(&r));
    checked(&pdf, &out, "");
    assert_eq!(exported(&out, &dir, ""), text, "export, import, export");
    // The output is never the input, and never replaces a file without --force.
    let r = run(["bookmarks", s(&pdf), "--import", s(&dir.join("exported.txt")), "-o", s(&out)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("already exists"), "{}", stderr(&r));
    let r = run(["bookmarks", s(&pdf), "--import", s(&dir.join("exported.txt")), "-o", s(&pdf), "--force"]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("same as an input"), "{}", stderr(&r));
    let r = run(["bookmarks", s(&pdf), "--export", s(&dir.join("exported.txt"))]);
    assert_eq!(code(&r), 1, "an export does not replace a file either");
}

#[test]
fn a_text_with_a_byte_order_mark_dos_line_ends_and_blank_lines_is_read_and_a_bad_line_is_named() {
    let dir = out_dir("lines");
    let pdf = gongwen();
    let txt = dir.join("in.txt");
    let out = dir.join("o.pdf");
    let write = |bytes: &[u8]| std::fs::write(&txt, bytes).unwrap();
    // BOM, CRLF, a blank line, Chinese: fine.
    write("\u{feff}第一章 总则\t1\r\n\r\n\t第一节\t2\r\n\t\t更深一层\t2\r\n第二章\t9".as_bytes());
    let r = run(["bookmarks", s(&pdf), "--import", s(&txt), "-o", s(&out), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert_eq!(exported(&out, &dir, ""), "第一章 总则\t1\n\t第一节\t2\n\t\t更深一层\t2\n第二章\t9\n");
    checked(&pdf, &out, "");
    // The bad ones: the line is named, nothing is written.
    let bad = |text: &str, line: &str| {
        write(text.as_bytes());
        let _ = std::fs::remove_file(&out);
        let r = run(["bookmarks", s(&pdf), "--import", s(&txt), "-o", s(&out)]);
        assert_eq!(code(&r), 1, "{text:?}");
        assert!(stderr(&r).contains(line), "{text:?}: {}", stderr(&r));
        assert!(!out.exists(), "{text:?}: a file was written");
    };
    bad("好\t1\n没有页码\n", "line 2");
    bad("好\t1\n页码不对\tx\n", "line 2: 'x' is not a page number");
    bad("好\t1\n\n\n太大\t999\n", "line 4: page 999 is not in the file");
    bad("好\t0\n", "line 1: page 0");
    bad("\t好\t1\n", "line 1: the level jumps");
    bad("好\t1\n\t\t跳级\t1\n", "line 2: the level jumps");
    bad("\t\n   \t1\n", "line 2: the title is empty");
    // Levels are tabs: spaces for a level are refused (they would be taken for the start of the title), and so is a tab inside the title (a
    // line with a tab after the page number would read the number as part of the title).
    bad("好\t1\n    二级\t1\n", "line 2: the line starts with a space");
    bad("\u{3000}好\t1\n", "line 1: the line starts with a space");
    bad("好\t1\n\t 二级\t1\n", "line 2: the line starts with a space");
    bad("好\t3\t\n", "line 1: there is a tab inside the title");
    bad("好\t1\n二\t三\t1\n", "line 2: there is a tab inside the title");
    bad(&format!("{}\t1\n", "字".repeat(301)), "line 1: the title is more than 300");
    bad("", "has no bookmarks");
    bad("\n  \n", "has no bookmarks");
    // Blanks at the ends of a title and of a page number are taken off.
    write("好 \u{a0}\t1\n\t二 \t 2 \n".as_bytes());
    let _ = std::fs::remove_file(&out);
    let r = run(["bookmarks", s(&pdf), "--import", s(&txt), "-o", s(&out)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert_eq!(exported(&out, &dir, ""), "好\t1\n\t二\t2\n");
    // Not UTF-8, and a file that is too big (the check is of its size before it is read).
    std::fs::write(&txt, [0xFFu8, 0xFE, b'a']).unwrap();
    let r = run(["bookmarks", s(&pdf), "--import", s(&txt), "-o", s(&out)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("not UTF-8"), "{}", stderr(&r));
    std::fs::write(&txt, vec![b'a'; 4 * 1024 * 1024 + 1]).unwrap();
    let r = run(["bookmarks", s(&pdf), "--import", s(&txt), "-o", s(&out)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("too big"), "{}", stderr(&r));
    // Too many levels and too many bookmarks.
    let deep: String = (0..33).map(|d| format!("{}d\t1\n", "\t".repeat(d))).collect();
    bad(&deep, "levels");
    let many: String = (0..20_001).map(|i| format!("b{i}\t1\n")).collect();
    bad(&many, "20000");
    // A file that is not there.
    let r = run(["bookmarks", s(&pdf), "--import", s(&dir.join("no-such.txt")), "-o", s(&out)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("cannot read"), "{}", stderr(&r));
}

/// A classic file of one page and the given objects, which are the bookmarks (and anything else the test wants in the file).
fn one_page_with(dir: &Path, name: &str, outline: &str, extra: Vec<(u32, String)>) -> PathBuf {
    let mut objects: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>".to_string()),
        (10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> >>".to_string()),
        (5, outline.to_string()),
    ];
    objects.extend(extra);
    let mut bytes: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in &objects {
        offsets.insert(*n, bytes.len());
        bytes.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|x| x.0).max().unwrap() + 1;
    let at = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(off) => bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_bookmark_with_no_title_is_exported_as_a_word_and_the_text_is_taken_back() {
    let dir = out_dir("untitled");
    let items = vec![
        (100, "<< /Title () /Parent 5 0 R /Next 101 0 R /Dest [10 0 R /Fit] >>".to_string()),
        // A no-break space and nothing else (UTF-16).
        (101, "<< /Title <FEFF00A0> /Parent 5 0 R /Prev 100 0 R /Next 102 0 R /Dest [10 0 R /Fit] >>".to_string()),
        (102, "<< /Title (Named) /Parent 5 0 R /Prev 101 0 R /Dest [10 0 R /Fit] >>".to_string()),
    ];
    let pdf = one_page_with(&dir, "untitled.pdf", "<< /Type /Outlines /First 100 0 R /Last 102 0 R /Count 3 >>", items);
    let text = exported(&pdf, &dir, "");
    assert_eq!(text, "(untitled)\t1\n(untitled)\t1\nNamed\t1\n");
    // The text is taken back, whole.
    let out = dir.join("back.pdf");
    let r = run(["bookmarks", s(&pdf), "--import", s(&dir.join("exported.txt")), "-o", s(&out)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    checked(&pdf, &out, "");
    assert_eq!(exported(&out, &dir, ""), text, "export, import, export");
}

#[test]
fn bookmarks_beyond_what_is_read_are_warned_of_and_an_import_replaces_them() {
    let dir = out_dir("limits");
    // A chain of 40 bookmarks, each inside the one before; 32 levels are read.
    let levels = 40usize;
    let mut items = Vec::new();
    for i in 0..levels {
        let parent = if i == 0 { 5 } else { 99 + i };
        let below = if i + 1 < levels { format!(" /First {} 0 R /Last {} 0 R /Count {}", 101 + i, 101 + i, levels - i - 1) } else { String::new() };
        items.push((100 + i as u32, format!("<< /Title (Level {i}) /Parent {parent} 0 R{below} /Dest [10 0 R /Fit] >>")));
    }
    let pdf = one_page_with(&dir, "deep.pdf", &format!("<< /Type /Outlines /First 100 0 R /Last 100 0 R /Count {levels} >>"), items);

    let txt = dir.join("exported.txt");
    let r = run(["bookmarks", s(&pdf), "--export", s(&txt), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stderr(&r).contains("warning") && stderr(&r).contains("more bookmarks than are read") && stderr(&r).contains("not in the text"), "{}", stderr(&r));
    assert_eq!(std::fs::read_to_string(&txt).unwrap().lines().count(), 32);

    // An import takes their place, and says so.
    let text = "第一章\t1\n\t第一节\t1\n第二章\t1\n";
    let new = dir.join("new.txt");
    std::fs::write(&new, text).unwrap();
    let out = dir.join("replaced.pdf");
    let r = run(["bookmarks", s(&pdf), "--import", s(&new), "-o", s(&out)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stderr(&r).contains("warning") && stderr(&r).contains("all of them are replaced"), "{}", stderr(&r));
    checked(&pdf, &out, "");
    // (What is exported is the text imported, and nothing of the 40 levels: there is no warning now.)
    let r = run(["bookmarks", s(&out), "--export", s(&txt), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(!stderr(&r).contains("warning"), "{}", stderr(&r));
    assert_eq!(std::fs::read_to_string(&txt).unwrap(), text);
}

#[test]
fn auto_makes_bookmarks_from_the_headings_of_a_file_that_has_none() {
    let dir = out_dir("auto");
    let pdf = common::public_file("zh/gongwen/gongwen-2024-nicheng-work-report.pdf");
    assert_eq!(exported(&pdf, &dir, ""), "", "a file with no bookmarks exports nothing");
    let out = dir.join("auto.pdf");
    let r = run(["bookmarks", s(&pdf), "--auto", "-o", s(&out)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    checked(&pdf, &out, "");
    let text = exported(&out, &dir, "");
    assert!(text.lines().count() >= 3 && text.contains("政府工作报告"), "{text}");
    // At most three levels.
    assert!(text.lines().all(|l| l.chars().take_while(|&c| c == '\t').count() < 3));
    // It replaces what is there.
    let again = dir.join("again.pdf");
    let r = run(["bookmarks", s(&gongwen()), "--auto", "-o", s(&again)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("replacing 18"), "{}", stdout(&r));
    // A file with no text has no headings.
    let image = dir.join("image.pdf");
    let jpeg = common::workspace_root().join("crates").join("qingpdf-core").join("tests").join("fixtures");
    let first = std::fs::read_dir(jpeg).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "jpg")).unwrap();
    assert_eq!(code(&run(["img2pdf", s(&first), "-o", s(&image), "--force"])), 0);
    let r = run(["bookmarks", s(&image), "--auto", "-o", s(&dir.join("none.pdf"))]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("no headings"), "{}", stderr(&r));
}

#[test]
fn the_book_of_468_pages_gets_its_bookmarks_in_two_seconds() {
    let pdf = common::corpus_root().join("local/zh/ebook/ebook-wikisource-yijikao-468p.pdf");
    if !pdf.is_file() {
        println!("SKIPPED: {} is not here", pdf.display());
        return;
    }
    let dir = out_dir("book");
    let out = dir.join("book.pdf");
    // (The best of three: the machine this runs on may be busy with something else for a moment.)
    let mut best = std::time::Duration::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        let r = run(["bookmarks", s(&pdf), "--auto", "-o", s(&out), "--force"]);
        let took = started.elapsed();
        assert_eq!(code(&r), 0, "{}", stderr(&r));
        println!("bookmarks --auto on 468 pages: {took:?} ({})", stdout(&r).trim());
        best = best.min(took);
        if best.as_secs_f64() <= 2.0 {
            break;
        }
    }
    assert!(best.as_secs_f64() <= 2.0, "{best:?}");
    checked(&pdf, &out, "");
}

#[test]
fn an_encrypted_file_keeps_its_encryption_and_the_rights_decide() {
    let dir = out_dir("encrypted");
    let pdf = gongwen();
    let (open, owner) = (dir.join("aes.pdf"), dir.join("locked.pdf"));
    let r = run(["encrypt", s(&pdf), "-o", s(&open), "--user-password", "u-1", "--owner-password", "o-1"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let text = exported(&open, &dir, "u-1");
    assert!(text.contains("二、主要任务\t2"));
    let out = dir.join("aes-out.pdf");
    let r = run(["bookmarks", s(&open), "--import", s(&dir.join("exported.txt")), "-o", s(&out), "--password", "u-1"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    checked(&open, &out, "u-1");
    // Without the password: asked for it.
    let r = run(["bookmarks", s(&open), "--export", s(&dir.join("x.txt"))]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("needs a password"), "{}", stderr(&r));
    // Neither bit 11 nor bit 4: refused, unless the owner password is given.
    let r = run(["encrypt", s(&pdf), "-o", s(&owner), "--owner-password", "o-2", "--no-assemble", "--no-modify"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    let locked_out = dir.join("locked-out.pdf");
    let r = run(["bookmarks", s(&owner), "--import", s(&dir.join("exported.txt")), "-o", s(&locked_out)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("did not allow changing its bookmarks") && stderr(&r).contains("owner password"), "{}", stderr(&r));
    assert!(!locked_out.exists());
    let r = run(["bookmarks", s(&owner), "--import", s(&dir.join("exported.txt")), "-o", s(&locked_out), "--password", "o-2"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    checked(&owner, &locked_out, "o-2");
    // Either bit is enough.
    for (flag, name) in [("--no-assemble", "no-assemble"), ("--no-modify", "no-modify")] {
        let one = dir.join(format!("{name}.pdf"));
        assert_eq!(code(&run(["encrypt", s(&pdf), "-o", s(&one), "--owner-password", "o-3", flag])), 0);
        let made = dir.join(format!("{name}-out.pdf"));
        let r = run(["bookmarks", s(&one), "--import", s(&dir.join("exported.txt")), "-o", s(&made)]);
        assert_eq!(code(&r), 0, "{flag}: {}", stderr(&r));
    }
}
