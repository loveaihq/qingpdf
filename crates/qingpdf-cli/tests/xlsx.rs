//! `qingpdf xlsx` (5d), run as a user would: arguments in, a file and an exit code out; and the speed on a hundred pages of a statement.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_qingpdf")).args(args).output().expect("cannot run qingpdf")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Takes the folder of a test away when the test is over, whether it passed or not.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qingpdf-xlsx-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A file of `pages` pages, each with a statement of `rows` rows of five columns (no lines), written by hand.
fn statement_pdf(pages: usize, rows: usize) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    for p in 0..pages {
        let mut c = String::new();
        for r in 0..=rows {
            let y = 780 - 14 * r;
            let cells = if r == 0 {
                ["Date".to_string(), "Memo".to_string(), "Amount".to_string(), "Balance".to_string(), "Account".to_string()]
            } else {
                [format!("2024-{:02}-{:02}", 1 + p % 12, 1 + r % 28), format!("Payment {r}"), format!("-{r},{:03}.50", r * 7 % 1000), format!("{},{:03}.25", 10 + r, r), "6222021234567890123".to_string()]
            };
            for (x, cell) in [50, 130, 260, 340, 430].iter().zip(&cells) {
                c.push_str(&format!("BT /F1 9 Tf {x} {y} Td ({cell}) Tj ET\n"));
            }
        }
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + 2 * p));
        objects.push(format!("<< /Length {} >>\nstream\n{c}endstream", c.len()));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

#[test]
fn xlsx_writes_an_excel_file_and_says_what_it_found() {
    let dir = out_dir("basic");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("zh/lunwen/lunwen-arxiv-2403.14268-word-tc.pdf");
    let out = dir.join("a.xlsx");
    let o = run(["xlsx", s(&input), "--pages", "3-4", "-o", s(&out)]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let said = stdout(&o);
    assert!(said.contains("wrote") && said.contains("2 pages") && said.contains("3 tables") && said.contains("cells"), "{said}");
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04") && bytes.windows(20).any(|w| w == b"xl/sharedStrings.xml"));
    // No file is left beside it (the temporary file is renamed over the output).
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["a.xlsx".to_string()]);
}

#[test]
fn a_file_with_no_table_is_written_with_a_warning() {
    let dir = out_dir("none");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("zh/gongwen/gongwen-2024-nicheng-work-report.pdf");
    let out = dir.join("a.xlsx");
    let o = run(["xlsx", s(&input), "--pages", "1-2", "-o", s(&out)]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).contains("no table"), "{}", stderr(&o));
    assert!(stdout(&o).contains("0 tables"), "{}", stdout(&o));
}

#[test]
fn xlsx_does_not_overwrite_and_says_what_is_wrong_with_the_command_line() {
    let dir = out_dir("rules");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("zh/lunwen/lunwen-arxiv-2403.14268-word-tc.pdf");
    let out = dir.join("a.xlsx");
    assert_eq!(code(&run(["xlsx", s(&input), "--pages", "3", "-o", s(&out)])), 0);
    let again = run(["xlsx", s(&input), "--pages", "3", "-o", s(&out)]);
    assert_eq!(code(&again), 1);
    assert!(stderr(&again).contains("--force"), "{}", stderr(&again));
    assert_eq!(code(&run(["xlsx", s(&input), "--pages", "3", "-o", s(&out), "--force"])), 0);
    // The output is never the input.
    assert_eq!(code(&run(["xlsx", s(&input), "--pages", "3", "-o", s(&input), "--force"])), 1);
    // A page that is not there; no -o; an option that xlsx does not have.
    let o = run(["xlsx", s(&input), "--pages", "99", "-o", s(&dir.join("b.xlsx"))]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("page"), "{}", stderr(&o));
    assert!(!dir.join("b.xlsx").exists());
    assert_eq!(code(&run(["xlsx", s(&input)])), 2);
    assert_eq!(code(&run(["xlsx", s(&input), "--dpi", "100", "-o", s(&dir.join("c.xlsx"))])), 2);
    // The help says what it does.
    let help = run(["xlsx", "--help"]);
    assert_eq!(code(&help), 0);
    assert!(stdout(&help).contains("qingpdf xlsx") && stdout(&help).contains("Excel"), "{}", stdout(&help));
    assert!(stdout(&run(["--help"])).contains("xlsx"));
}

#[test]
fn an_encrypted_file_needs_its_password() {
    let dir = out_dir("password");
    let _cleanup = Cleanup(dir.clone());
    let input = common::public_file("encrypted/bug_644.pdf");
    let out = dir.join("a.xlsx");
    let without = run(["xlsx", s(&input), "-o", s(&out)]);
    assert_eq!(code(&without), 1);
    assert!(stderr(&without).contains("password"), "{}", stderr(&without));
    assert!(!out.exists());
    let with = run(["xlsx", s(&input), "--password", "a", "-o", s(&out)]);
    assert_eq!(code(&with), 0, "{}", stderr(&with));
    assert!(std::fs::read(&out).unwrap().starts_with(b"PK"));
}

#[test]
fn a_hundred_pages_of_a_statement_take_no_more_than_three_seconds() {
    // (The statement goes on from page to page, with its heading on every page: one sheet of 4,501 rows.)
    let dir = out_dir("speed");
    let _cleanup = Cleanup(dir.clone());
    let big = dir.join("big.pdf");
    std::fs::write(&big, statement_pdf(100, 45)).unwrap();
    let out = dir.join("big.xlsx");
    let started = Instant::now();
    let o = run(["xlsx", s(&big), "--pages", "1-100", "-o", s(&out)]);
    let took = started.elapsed();
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("100 pages") && stdout(&o).contains(", 1 table,") && stdout(&o).contains("22505 cells"), "{}", stdout(&o));
    assert!(took.as_secs_f64() <= 3.0, "{took:?}");
    eprintln!("100 pages: {took:?}");
}
