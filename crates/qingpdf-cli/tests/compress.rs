//! `qingpdf compress`, run as a user would: arguments in, files and exit codes out. What it writes is checked the way the engine's tests
//! check it (`qpdf --check`, the text of the pages is as it was, an encrypted file keeps its passwords); the engine's own tests and
//! `tests/tools/compress_check.py` (PDFium, the whole corpus, the speed) go through the combinations.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};

use runner::{code, run, s, stderr, stdout};

fn flyer() -> PathBuf {
    common::corpus_root().join("public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf")
}

fn text_of(file: &Path, password: &str) -> String {
    let out = run(["text", s(file), "--password", password]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    stdout(&out)
}

#[test]
fn compress_has_help_and_wrong_command_lines_exit_with_2() {
    assert!(stdout(&run(["--help"])).contains("compress"));
    let help = stdout(&run(["compress", "--help"]));
    for word in ["--preset", "--lossless", "high", "balanced", "small", "--password", "--force", "never bigger"] {
        assert!(help.contains(word), "the help does not mention {word}");
    }
    let dir = common::fresh_out_dir("cli-compress-usage");
    let (input, output) = (flyer(), dir.join("o.pdf"));
    let bad = |extra: &[&str]| {
        let mut args = vec!["compress", s(&input), "-o", s(&output)];
        args.extend_from_slice(extra);
        run(args)
    };
    assert_eq!(code(&bad(&["--preset", "tiny"])), 2);
    assert_eq!(code(&bad(&["--preset"])), 2);
    assert_eq!(code(&bad(&["--lossless=yes"])), 2);
    assert_eq!(code(&bad(&["--preset", "small", "--preset", "high"])), 2);
    assert!(!output.exists());
    assert_eq!(code(&run(["compress", s(&input)])), 2, "-o is required");
    assert_eq!(code(&run(["compress", "-o", s(&output)])), 2, "an input is required");
    assert_eq!(code(&run(["info", s(&input), "--lossless"])), 2, "the options belong to compress");
    assert_eq!(code(&run(["compress", s(&input), "-o", s(&input)])), 1, "the output is not the input");
}

#[test]
fn a_file_is_written_smaller_and_the_sizes_are_told() {
    let dir = common::fresh_out_dir("cli-compress");
    let input = flyer();
    let before = std::fs::metadata(&input).unwrap().len();
    let want = text_of(&input, "");
    let sizes: Vec<(&str, Vec<&str>)> = vec![("lossless", vec!["--lossless"]), ("balanced", vec![]), ("small", vec!["--preset", "small"]), ("high", vec!["--preset", "high"])];
    let mut got = Vec::new();
    for (name, extra) in &sizes {
        let output = dir.join(format!("{name}.pdf"));
        let mut args = vec!["compress", s(&input), "-o", s(&output)];
        args.extend(extra.iter().copied());
        let out = run(args);
        assert_eq!(code(&out), 0, "{name}: {}", stderr(&out));
        let said = stdout(&out);
        let size = std::fs::metadata(&output).unwrap().len();
        assert!(said.contains("wrote") && said.contains("smaller") && said.contains(&group(size)), "{name}: {said}");
        assert!(said.contains(&group(before)), "{name}: {said}");
        assert!(size < before, "{name}: {size} of {before}");
        assert_eq!(text_of(&output, ""), want, "{name}");
        if let Some(qpdf) = common::find_qpdf() {
            let verdict = common::qpdf_check(&qpdf, &output);
            assert_eq!(verdict.code, 0, "{name}: {}", verdict.text);
        }
        got.push(size);
    }
    // The pictures are given up more and more: lossless, high, balanced, small.
    assert!(got[0] > got[3] && got[3] > got[1] && got[1] > got[2], "{got:?}");
    // The output is never overwritten without --force.
    let output = dir.join("small.pdf");
    let again = run(["compress", s(&input), "-o", s(&output)]);
    assert_eq!(code(&again), 1);
    assert!(stderr(&again).contains("--force"), "{}", stderr(&again));
    assert_eq!(code(&run(["compress", s(&input), "-o", s(&output), "--force"])), 0);
}

/// 1234567 as `1,234,567`.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[test]
fn a_file_that_cannot_get_smaller_is_copied_and_the_command_says_so() {
    let dir = common::fresh_out_dir("cli-compress-kept");
    let tiny = dir.join("tiny.pdf");
    // (The smallest file there is, with a real cross-reference table.)
    let mut bytes = b"%PDF-1.4
".to_vec();
    let mut at = Vec::new();
    for (n, body) in [(1, "<< /Type /Catalog /Pages 2 0 R >>"), (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"), (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>")] {
        at.push(bytes.len());
        bytes.extend_from_slice(format!("{n} 0 obj
{body}
endobj
").as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref
0 4
0000000000 65535 f 
");
    for offset in at {
        bytes.extend_from_slice(format!("{offset:010} 00000 n 
").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer
<< /Size 4 /Root 1 0 R >>
startxref
{xref}
%%EOF
").as_bytes());
    std::fs::write(&tiny, bytes).unwrap();
    let output = dir.join("out.pdf");
    let out = run(["compress", s(&tiny), "-o", s(&output)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("no smaller than the input") && stdout(&out).contains("copy of the input"), "{}", stdout(&out));
    assert_eq!(std::fs::read(&output).unwrap(), std::fs::read(&tiny).unwrap());
}

#[test]
fn an_encrypted_file_keeps_its_passwords_and_a_signed_file_is_refused() {
    let dir = common::fresh_out_dir("cli-compress-encrypted");
    let input = flyer();
    let locked = dir.join("locked.pdf");
    let out = run(["encrypt", s(&input), "-o", s(&locked), "--user-password", "open-1", "--owner-password", "owner-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // Without the password: refused. With it: written, and the passwords still open it, and it is still encrypted.
    let output = dir.join("out.pdf");
    let no_pw = run(["compress", s(&locked), "-o", s(&output)]);
    assert_eq!(code(&no_pw), 1);
    assert!(stderr(&no_pw).contains("password"), "{}", stderr(&no_pw));
    let out = run(["compress", s(&locked), "-o", s(&output), "--password", "open-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!stdout(&out).contains("open-1") && !stderr(&out).contains("open-1"));
    assert!(std::fs::metadata(&output).unwrap().len() < std::fs::metadata(&locked).unwrap().len());
    assert_eq!(code(&run(["text", s(&output)])), 1, "it still needs a password");
    assert_eq!(text_of(&output, "open-1"), text_of(&input, ""));
    assert_eq!(text_of(&output, "owner-2"), text_of(&input, ""));
    assert_eq!(code(&run(["text", s(&output), "--password", "wrong"])), 1);
    let info = stdout(&run(["info", s(&output), "--password", "owner-2"]));
    assert!(info.contains("AES-256") && info.contains("Object streams:      yes"), "{info}");
    // A signed file.
    let signed = common::corpus_root().join("public/incremental/signature_reason.pdf");
    let refused = run(["compress", s(&signed), "-o", s(&dir.join("signed.pdf"))]);
    assert_eq!(code(&refused), 1);
    assert!(stderr(&refused).contains("signature") && !dir.join("signed.pdf").exists(), "{}", stderr(&refused));
}

#[test]
fn a_file_that_may_not_be_changed_is_compressed_without_loss_for_the_user_password_and_with_pictures_only_for_the_owner() {
    let dir = common::fresh_out_dir("cli-compress-no-modify");
    let input = flyer();
    let locked = dir.join("no-modify.pdf");
    let out = run(["encrypt", s(&input), "-o", s(&locked), "--user-password", "open-1", "--owner-password", "owner-2", "--no-modify"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let output = dir.join("out.pdf");
    // The pictures are to be changed: the user password is not enough.
    let refused = run(["compress", s(&locked), "-o", s(&output), "--password", "open-1"]);
    assert_eq!(code(&refused), 1);
    assert!(stderr(&refused).contains("did not allow changing its content") && !output.exists(), "{}", stderr(&refused));
    // Only what loses nothing: allowed, and the file keeps its passwords and its restrictions.
    let out = run(["compress", s(&locked), "-o", s(&output), "--password", "open-1", "--lossless"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(std::fs::metadata(&output).unwrap().len() < std::fs::metadata(&locked).unwrap().len());
    assert_eq!(text_of(&output, "open-1"), text_of(&input, ""));
    assert_eq!(code(&run(["text", s(&output), "--password", "wrong"])), 1);
    // The owner password: the pictures too.
    let out = run(["compress", s(&locked), "-o", s(&output), "--password", "owner-2", "--force"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}
