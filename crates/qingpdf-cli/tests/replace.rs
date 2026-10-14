//! `qingpdf replace` (5a), run as a user would: arguments in, files and exit codes out. What it writes is checked the way the engine's
//! tests check content (the original bytes are a prefix, `qpdf --check` where qpdf is there, our own reader extracts it), and the
//! speed on a file of five hundred pages is measured.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use runner::{code, run, s, stderr, stdout};

fn have_font() -> bool {
    ["arial.ttf", "LiberationSans-Regular.ttf", "DejaVuSans.ttf"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file() || Path::new("/usr/share/fonts/truetype/dejavu").join(f).is_file() || Path::new("/usr/share/fonts/truetype/liberation").join(f).is_file())
}

fn out_dir(name: &str) -> PathBuf {
    let dir = common::workspace_root().join("tests").join("out").join("replace").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file of `pages` pages of 300 by 400 points that share one `/Resources` (a font) and inherit their `/MediaBox`; each page has a
/// content stream of its own, `page N` and a word drawn in a piece of its own, kerned.
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
    put(&mut out, 4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string());
    for i in 0..pages {
        let text = format!("BT /Helv 14 Tf 30 350 Td (page {}) Tj 0 -16 Td [(The) -250 (quick) -250 (brown) -250 (fox)] TJ ET", i + 1);
        put(&mut out, 10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i));
        put(&mut out, 11 + 2 * i, format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
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
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn replace_changes_the_text_writes_the_original_first_and_says_what_it_did() {
    if !have_font() {
        return;
    }
    let dir = out_dir("basic");
    let input = dir.join("in.pdf");
    let original = many_pages(5);
    std::fs::write(&input, &original).unwrap();
    let output = dir.join("out.pdf");
    let r = run(["replace", s(&input), "--find", "quick brown", "--replace", "slow red", "--pages", "2-3", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("replaced 2 places on 2 pages"), "{}", stdout(&r));
    let written = std::fs::read(&output).unwrap();
    assert!(written.starts_with(&original) && written.len() > original.len());
    let text = run(["text", s(&output)]);
    let text = stdout(&text);
    assert_eq!(text.matches("slow red").count(), 2, "{text}");
    assert_eq!(text.matches("quick brown").count(), 3, "{text}");
    if let Some(qpdf) = common::find_qpdf() {
        let check = Command::new(qpdf).arg("--check").arg(&output).output().unwrap();
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stdout));
    }
    // Nothing found: exit status 1, no file written. Not given what is needed: 2.
    let missing = dir.join("none.pdf");
    let r = run(["replace", s(&input), "--find", "absent", "--replace", "x", "-o", s(&missing)]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("was not found") && !missing.exists(), "{}", stderr(&r));
    for args in [vec!["replace", s(&input), "--replace", "x", "-o", s(&missing)], vec!["replace", s(&input), "--find", "a", "-o", s(&missing)], vec!["replace", s(&input), "--find", "a", "--replace", "b"], vec!["replace", s(&input), "--find", "", "--replace", "b", "-o", s(&missing)]] {
        let r = run(&args);
        assert_eq!(code(&r), 2, "{args:?}: {}", stderr(&r));
    }
    // The text to find and the text to put there are the same; a line end; a page that is not there; the input as the output.
    for args in [vec!["--find", "a", "--replace", "a"], vec!["--find", "a", "--replace", "b\nc"], vec!["--find", "a", "--replace", "b", "--pages", "9"]] {
        let r = run(["replace", s(&input)].into_iter().chain(args.iter().copied()).chain(["-o", s(&missing)]));
        assert_eq!(code(&r), 1, "{args:?}: {}", stderr(&r));
    }
    let r = run(["replace", s(&input), "--find", "quick", "--replace", "x", "-o", s(&input), "--force"]);
    assert_ne!(code(&r), 0);
    assert_eq!(std::fs::read(&input).unwrap(), original, "the input is never touched");
    // The help names the command.
    let help = run(["replace", "--help"]);
    assert!(stdout(&help).contains("--find") && stdout(&run(["--help"])).contains("replace"));
}

#[test]
fn five_hundred_pages_are_searched_and_changed_in_a_second_or_two() {
    if !have_font() {
        return;
    }
    let dir = out_dir("many");
    let input = dir.join("in.pdf");
    std::fs::write(&input, many_pages(500)).unwrap();
    let output = dir.join("out.pdf");
    let started = Instant::now();
    let r = run(["replace", s(&input), "--find", "quick", "--replace", "fast", "-o", s(&output), "--force"]);
    let took = started.elapsed();
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("replaced 500 places on 500 pages"), "{}", stdout(&r));
    assert!(took.as_secs_f64() < 3.0, "{took:?}");
    let text = stdout(&run(["text", s(&output), "--pages", "250"]));
    assert!(text.contains("The fast brown fox"), "{text}");
}

#[test]
fn an_encrypted_file_is_changed_with_its_own_key_and_a_file_that_may_not_be_changed_is_refused() {
    if !have_font() {
        return;
    }
    let Some(qpdf) = common::find_qpdf() else { return };
    let dir = out_dir("encrypted");
    let plain = dir.join("plain.pdf");
    std::fs::write(&plain, many_pages(3)).unwrap();
    let make = |name: &str, extra: &[&str]| {
        let path = dir.join(name);
        let mut cmd = Command::new(&qpdf);
        cmd.args(["--encrypt", "userpw", "ownerpw", "256"]).args(extra).args(["--", s(&plain), s(&path)]);
        assert!(cmd.output().unwrap().status.success());
        path
    };
    let aes = make("aes256.pdf", &[]);
    let output = dir.join("out.pdf");
    let r = run(["replace", s(&aes), "--password", "userpw", "--find", "quick", "--replace", "swift", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stdout(&r).contains("replaced 3 places"));
    let text = stdout(&run(["text", s(&output), "--password", "userpw"]));
    assert_eq!(text.matches("The swift brown fox").count(), 3, "{text}");
    let check = Command::new(&qpdf).args(["--check", "--password=userpw"]).arg(&output).output().unwrap();
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stdout));
    // No password: asked for one. The author did not allow changing it: refused, unless the owner password is given.
    let r = run(["replace", s(&aes), "--find", "quick", "--replace", "swift", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("password"), "{}", stderr(&r));
    let locked = make("nomodify.pdf", &["--modify=none"]);
    let r = run(["replace", s(&locked), "--password", "userpw", "--find", "quick", "--replace", "swift", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 1);
    assert!(stderr(&r).contains("did not allow changing"), "{}", stderr(&r));
    let r = run(["replace", s(&locked), "--password", "ownerpw", "--find", "quick", "--replace", "swift", "-o", s(&output), "--force"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
}
