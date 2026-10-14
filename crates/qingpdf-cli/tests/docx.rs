//! `qingpdf docx` (5b), run as a user would: arguments in, a file and an exit code out; and the speed on a hundred pages of a paper.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::PathBuf;
use std::time::Instant;

use runner::{code, run, s, stderr, stdout};

/// Takes the folder of a test away when the test is over, whether it passed or not.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qingpdf-docx-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn docx_writes_a_word_file_and_says_what_it_found() {
    let dir = out_dir("basic");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("zh/gongwen/gongwen-2024-nicheng-work-report.pdf");
    let out = dir.join("a.docx");
    let o = run(["docx", s(&input), "--pages", "1-2", "-o", s(&out)]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let said = stdout(&o);
    assert!(said.contains("wrote") && said.contains("2 pages") && said.contains("paragraph") && said.contains("heading"), "{said}");
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04") && bytes.windows(19).any(|w| w == b"[Content_Types].xml"));
    // No file is left beside it (the temporary file is renamed over the output).
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["a.docx".to_string()]);
}

#[test]
fn docx_does_not_overwrite_and_says_what_is_wrong_with_the_command_line() {
    let dir = out_dir("rules");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("zh/gongwen/gongwen-2024-nicheng-work-report.pdf");
    let out = dir.join("a.docx");
    assert_eq!(code(&run(["docx", s(&input), "--pages", "1", "-o", s(&out)])), 0);
    let again = run(["docx", s(&input), "--pages", "1", "-o", s(&out)]);
    assert_eq!(code(&again), 1);
    assert!(stderr(&again).contains("--force"), "{}", stderr(&again));
    assert_eq!(code(&run(["docx", s(&input), "--pages", "1", "-o", s(&out), "--force"])), 0);
    // The output is never the input.
    let same = run(["docx", s(&input), "--pages", "1", "-o", s(&input), "--force"]);
    assert_eq!(code(&same), 1);
    // A page that is not there; no -o; an option that docx does not have.
    let o = run(["docx", s(&input), "--pages", "99", "-o", s(&dir.join("b.docx"))]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("page"), "{}", stderr(&o));
    assert!(!dir.join("b.docx").exists());
    assert_eq!(code(&run(["docx", s(&input)])), 2);
    assert_eq!(code(&run(["docx", s(&input), "--dpi", "100", "-o", s(&dir.join("c.docx"))])), 2);
    // The help says what it does.
    let help = run(["docx", "--help"]);
    assert_eq!(code(&help), 0);
    assert!(stdout(&help).contains("qingpdf docx") && stdout(&help).contains("Word"), "{}", stdout(&help));
    assert!(stdout(&run(["--help"])).contains("docx"));
}

#[test]
fn an_encrypted_file_needs_its_password() {
    let dir = out_dir("password");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("encrypted/bug_644.pdf");
    let out = dir.join("a.docx");
    let without = run(["docx", s(&input), "-o", s(&out)]);
    assert_eq!(code(&without), 1);
    assert!(stderr(&without).contains("password"), "{}", stderr(&without));
    assert!(!out.exists());
    let with = run(["docx", s(&input), "--password", "a", "-o", s(&out)]);
    assert_eq!(code(&with), 0, "{}", stderr(&with));
    assert!(std::fs::read(&out).unwrap().starts_with(b"PK"));
}

#[test]
fn a_hundred_pages_of_a_paper_take_no_more_than_three_seconds() {
    let dir = out_dir("speed");
    let _cleanup = Cleanup(dir.clone());
    let paper = common::public_file("zh/lunwen/lunwen-arxiv-2401.01717-word.pdf");
    // Eleven pages, ten times over.
    let big = dir.join("big.pdf");
    let mut args: Vec<&str> = vec!["merge"];
    let p = s(&paper).to_string();
    let copies: Vec<String> = (0..10).map(|_| p.clone()).collect();
    args.extend(copies.iter().map(String::as_str));
    args.extend(["-o", s(&big)]);
    assert_eq!(code(&run(&args)), 0);
    let out = dir.join("big.docx");
    let started = Instant::now();
    let o = run(["docx", s(&big), "--pages", "1-100", "-o", s(&out)]);
    let took = started.elapsed();
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("100 pages"), "{}", stdout(&o));
    assert!(took.as_secs_f64() <= 3.0, "{took:?}");
}
