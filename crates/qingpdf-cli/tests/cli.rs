//! The `qingpdf` program, run as a user would: arguments in, files and exit
//! codes out. Inputs are made with `img2pdf` from the JPEG fixtures, or taken
//! from the public corpus.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn fixtures() -> PathBuf {
    common::workspace_root().join("crates").join("qingpdf-core").join("tests").join("fixtures")
}

fn jpegs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(fixtures())
        .expect("fixtures folder")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .collect();
    v.sort();
    v
}

/// A file of the public corpus; the test fails if it is not there.
fn corpus(rel: &str) -> PathBuf {
    common::public_file(rel)
}

/// Make a PDF with one page per fixture JPEG (7 pages) using the tool itself.
fn make_seven_page_pdf(path: &Path) {
    let mut args: Vec<PathBuf> = vec!["img2pdf".into()];
    args.extend(jpegs());
    args.push("-o".into());
    args.push(path.into());
    args.push("--force".into());
    let out = run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

fn page_count_of(path: &Path) -> usize {
    let out = run([OsStr::new("info"), path.as_os_str()]);
    assert_eq!(code(&out), 0, "info {}: {}", path.display(), stderr(&out));
    let text = stdout(&out);
    let line = text.lines().find(|l| l.starts_with("Pages:")).expect("a Pages: line");
    line.trim_start_matches("Pages:").trim().parse().expect("a page count")
}

// --- help and usage ---------------------------------------------------------------------

#[test]
fn help_and_version() {
    let out = run(["--help"]);
    assert_eq!(code(&out), 0);
    let text = stdout(&out);
    for word in ["info", "merge", "split", "delete", "rotate", "text", "img2pdf", "--force"] {
        assert!(text.contains(word), "main help should mention {word}");
    }
    for command in ["info", "merge", "split", "delete", "rotate", "text", "img2pdf"] {
        let out = run([command, "--help"]);
        assert_eq!(code(&out), 0, "{command}");
        assert!(stdout(&out).contains(&format!("qingpdf {command}")), "{command}");
        let out = run(["help", command]);
        assert_eq!(code(&out), 0, "{command}");
    }
    let out = run(["--version"]);
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).starts_with("qingpdf 0."));
    // The split help explains --every and %d.
    let text = stdout(&run(["split", "--help"]));
    assert!(text.contains("--every") && text.contains("%d") && text.contains("1-3,5"));
}

#[test]
fn wrong_command_lines_exit_with_2() {
    let dir = common::fresh_out_dir("cli-usage");
    let a = dir.join("a.pdf");
    for args in [
        vec![],
        vec!["frobnicate"],
        vec!["info"],
        vec!["merge", "a.pdf", "-o", "x.pdf"],
        vec!["split", "a.pdf", "-o", "x.pdf"],
        vec!["delete", "a.pdf", "--pages", "1"],
        vec!["rotate", "a.pdf", "--angle", "45", "-o", "x.pdf"],
        vec!["img2pdf", "-o", "x.pdf"],
        vec!["info", "a.pdf", "--bogus"],
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).starts_with("qingpdf: "), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
    assert!(!a.exists());
}

// --- review findings ------------------------------------------------------------------------

/// Another name for the input's data (a hard link) must not be written into.
#[test]
fn an_output_that_is_a_hard_link_of_an_input_leaves_the_input_alone() {
    let dir = common::fresh_out_dir("cli-hardlink");
    let input = dir.join("in.pdf");
    make_seven_page_pdf(&input);
    let before = std::fs::read(&input).expect("read");
    let link = dir.join("link.pdf");
    std::fs::hard_link(&input, &link).expect("this file system cannot make a hard link");
    // The names differ, so the same-file check lets it through with --force.
    let out = run([OsStr::new("rotate"), input.as_os_str(), "--angle".as_ref(), "90".as_ref(), "-o".as_ref(), link.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(std::fs::read(&input).expect("read"), before, "the input was written into through its hard link");
    let after = std::fs::read(&link).expect("read");
    assert_ne!(after, before, "the output is the rotated file");
    assert_eq!(page_count_of(&link), 7);
    // Splitting into files named like the link does the same.
    let template = dir.join("part_%d.pdf");
    let first = dir.join("part_1.pdf");
    std::fs::hard_link(&input, &first).expect("hard link");
    let out = run([OsStr::new("split"), input.as_os_str(), "--every".as_ref(), "3".as_ref(), "-o".as_ref(), template.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(std::fs::read(&input).expect("read"), before);
    assert_eq!(page_count_of(&first), 3);
    // No temporary file is left behind.
    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A name that cannot be written leaves no half-written file and says why.
#[test]
fn a_failing_write_leaves_nothing_behind() {
    let dir = common::fresh_out_dir("cli-write-fails");
    let input = dir.join("in.pdf");
    make_seven_page_pdf(&input);
    let out = run([OsStr::new("rotate"), input.as_os_str(), "--angle".as_ref(), "90".as_ref(), "-o".as_ref(), dir.join("no such folder").join("o.pdf").as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("cannot write"), "{}", stderr(&out));
}

/// Control characters that come from a file are written out, not sent: the
/// escape that starts a terminal command is the worst of them.
#[test]
fn control_characters_from_a_file_do_not_reach_the_terminal() {
    let dir = common::fresh_out_dir("cli-escape");
    // A file with a proper cross-reference table, whose object 4 cannot be read:
    // it is a keyword with an escape and a bell in it.
    let bodies: [&[u8]; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 10 10] >>",
        b"<< /Type /Page /Parent 2 0 R /Foo 4 0 R >>",
        b"\x1bZZ\x07Q",
    ];
    let mut pdf: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for o in &offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    let input = dir.join("esc.pdf");
    std::fs::write(&input, &pdf).expect("write");
    let o = dir.join("o.pdf");
    let out = run([OsStr::new("rotate"), input.as_os_str(), "--angle".as_ref(), "90".as_ref(), "-o".as_ref(), o.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let raw = &out.stderr;
    assert!(
        raw.iter().all(|&b| b == b'\n' || (b >= 0x20 && b != 0x7f)),
        "a control character was printed: {:?}",
        String::from_utf8_lossy(raw)
    );
    assert!(stderr(&out).contains("\\x1bZZ\\x07Q"), "{}", stderr(&out));
}

// --- the commands ----------------------------------------------------------------------------

#[test]
fn img2pdf_info_split_delete_rotate_merge_on_a_seven_page_file() {
    let dir = common::fresh_out_dir("cli-commands");
    let seven = dir.join("seven.pdf");
    make_seven_page_pdf(&seven);
    assert_eq!(page_count_of(&seven), 7);

    // info: version, sizes in pt and mm, flags.
    let text = stdout(&run([OsStr::new("info"), seven.as_os_str()]));
    assert!(text.contains("PDF version:         1.4"), "{text}");
    assert!(text.contains("Encrypted:           no"), "{text}");
    assert!(text.contains("Cross-ref streams:   no") && text.contains("Object streams:      no"), "{text}");
    assert!(text.contains("Repaired:            no"), "{text}");
    // rgb_wide.jpg is 96x64 at 150 dpi's, A4 landscape page for wide images.
    assert!(text.contains("841.89 x 595.28 pt (297 x 210 mm)"), "{text}");
    // The portrait ones too (the EXIF 6 photo is stored wide but shown tall).
    assert!(text.contains("595.28 x 841.89 pt (210 x 297 mm)"), "{text}");

    // split --pages
    let part = dir.join("part.pdf");
    let out = run([OsStr::new("split"), seven.as_os_str(), "--pages".as_ref(), "1-3,7".as_ref(), "-o".as_ref(), part.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("(4 pages)"), "{}", stdout(&out));
    assert_eq!(page_count_of(&part), 4);

    // delete
    let fewer = dir.join("fewer.pdf");
    let out = run([OsStr::new("delete"), seven.as_os_str(), "--pages".as_ref(), "2,4-6".as_ref(), "-o".as_ref(), fewer.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&fewer), 3);

    // rotate: pages 1-2 by 90; info shows it.
    let turned = dir.join("turned.pdf");
    let out = run([
        OsStr::new("rotate"),
        seven.as_os_str(),
        "--pages".as_ref(),
        "1-2".as_ref(),
        "--angle".as_ref(),
        "90".as_ref(),
        "-o".as_ref(),
        turned.as_os_str(),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&run([OsStr::new("info"), turned.as_os_str()]));
    assert_eq!(text.matches("rotated 90").count(), 2, "{text}");
    // Without --pages every page turns; negative angles work.
    let all_turned = dir.join("all-turned.pdf");
    let out = run([OsStr::new("rotate"), turned.as_os_str(), "--angle".as_ref(), "-90".as_ref(), "-o".as_ref(), all_turned.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&run([OsStr::new("info"), all_turned.as_os_str()]));
    assert_eq!(text.matches("rotated 270").count(), 5, "{text}");
    assert_eq!(text.matches("rotated 90").count(), 0, "{text}");

    // merge: seven + part + fewer = 14 pages.
    let merged = dir.join("merged.pdf");
    let out = run([OsStr::new("merge"), seven.as_os_str(), part.as_os_str(), fewer.as_os_str(), "-o".as_ref(), merged.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&merged), 14);
}

#[test]
fn split_every_numbers_the_files_and_checks_every_name_first() {
    let dir = common::fresh_out_dir("cli-every");
    let seven = dir.join("seven.pdf");
    make_seven_page_pdf(&seven);
    let template = dir.join("out_%d.pdf");
    // One of the names is taken: nothing at all is written.
    std::fs::write(dir.join("out_2.pdf"), b"precious").expect("write");
    let out = run([OsStr::new("split"), seven.as_os_str(), "--every".as_ref(), "3".as_ref(), "-o".as_ref(), template.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("out_2.pdf") && stderr(&out).contains("--force"), "{}", stderr(&out));
    assert!(!dir.join("out_1.pdf").exists() && !dir.join("out_3.pdf").exists());
    assert_eq!(std::fs::read(dir.join("out_2.pdf")).expect("read"), b"precious");
    // With --force all three are written.
    let out = run([
        OsStr::new("split"),
        seven.as_os_str(),
        "--every".as_ref(),
        "3".as_ref(),
        "-o".as_ref(),
        template.as_os_str(),
        "--force".as_ref(),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&dir.join("out_1.pdf")), 3);
    assert_eq!(page_count_of(&dir.join("out_2.pdf")), 3);
    assert_eq!(page_count_of(&dir.join("out_3.pdf")), 1);
    assert!(!dir.join("out_4.pdf").exists());
}

#[test]
fn never_overwrites_without_force_and_never_over_an_input() {
    let dir = common::fresh_out_dir("cli-overwrite");
    let a = dir.join("a.pdf");
    make_seven_page_pdf(&a);
    let before = std::fs::read(&a).expect("read");
    let b = dir.join("b.pdf");
    std::fs::write(&b, b"not yours").expect("write");

    // An existing output: refused, untouched.
    let out = run([OsStr::new("delete"), a.as_os_str(), "--pages".as_ref(), "1".as_ref(), "-o".as_ref(), b.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("already exists") && stderr(&out).contains("--force"), "{}", stderr(&out));
    assert_eq!(std::fs::read(&b).expect("read"), b"not yours");
    // With --force: written.
    let out = run([OsStr::new("delete"), a.as_os_str(), "--pages".as_ref(), "1".as_ref(), "-o".as_ref(), b.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&b), 6);

    // The output may never be an input, with or without --force, however it is spelled.
    let sneaky = dir.join(".").join("a.pdf");
    for target in [&a, &sneaky] {
        for force in [false, true] {
            let mut args: Vec<&OsStr> = vec!["rotate".as_ref(), a.as_os_str(), "--angle".as_ref(), "90".as_ref(), "-o".as_ref(), target.as_os_str()];
            if force {
                args.push("--force".as_ref());
            }
            let out = run(args);
            assert_eq!(code(&out), 1, "{target:?} force={force}");
            assert!(stderr(&out).contains("same as an input"), "{}", stderr(&out));
        }
    }
    let out = run([OsStr::new("merge"), a.as_os_str(), b.as_os_str(), "-o".as_ref(), b.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("same as an input"), "{}", stderr(&out));
    let out = run([OsStr::new("img2pdf"), jpegs()[0].as_os_str(), "-o".as_ref(), jpegs()[0].as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 1);
    assert_eq!(std::fs::read(&a).expect("read"), before, "the input must be untouched");
    // A folder is not a file name.
    let out = run([OsStr::new("rotate"), a.as_os_str(), "--angle".as_ref(), "90".as_ref(), "-o".as_ref(), dir.as_os_str()]);
    assert_eq!(code(&out), 1);
}

#[test]
fn paths_with_chinese_characters_and_spaces_work_everywhere() {
    let base = common::fresh_out_dir("cli-中文 路径");
    let dir = base.join("子目录 with spaces");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let input = dir.join("输入 文件.pdf");
    let mut args: Vec<PathBuf> = vec!["img2pdf".into()];
    args.extend(jpegs().into_iter().take(3));
    args.extend(["-o".into(), input.clone()]);
    let out = run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("输入 文件.pdf"), "{}", stdout(&out));
    assert_eq!(page_count_of(&input), 3);

    let merged = dir.join("合并 结果.pdf");
    let out = run([OsStr::new("merge"), input.as_os_str(), input.as_os_str(), "-o".as_ref(), merged.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&merged), 6);

    let some = dir.join("提取 页面.pdf");
    let out = run([OsStr::new("split"), merged.as_os_str(), "--pages".as_ref(), "2-4".as_ref(), "-o".as_ref(), some.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&some), 3);

    let rest = dir.join("删除 后.pdf");
    let out = run([OsStr::new("delete"), merged.as_os_str(), "--pages".as_ref(), "1".as_ref(), "-o".as_ref(), rest.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&rest), 5);

    let turned = dir.join("旋转 后.pdf");
    let out = run([OsStr::new("rotate"), rest.as_os_str(), "--angle".as_ref(), "180".as_ref(), "-o".as_ref(), turned.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&run([OsStr::new("info"), turned.as_os_str()]));
    assert!(text.contains("旋转 后.pdf") && text.matches("rotated 180").count() == 5, "{text}");

    let template = dir.join("拆分 %d 号.pdf");
    let out = run([OsStr::new("split"), merged.as_os_str(), "--every".as_ref(), "4".as_ref(), "-o".as_ref(), template.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&dir.join("拆分 1 号.pdf")), 4);
    assert_eq!(page_count_of(&dir.join("拆分 2 号.pdf")), 2);

    // And an image whose own name is Chinese.
    let photo = dir.join("照片 一.jpg");
    std::fs::copy(&jpegs()[0], &photo).expect("copy");
    let out_pdf = dir.join("照片.pdf");
    let out = run([OsStr::new("img2pdf"), photo.as_os_str(), "-o".as_ref(), out_pdf.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(page_count_of(&out_pdf), 1);
}

#[test]
fn errors_give_exit_code_1_and_a_clear_message() {
    let dir = common::fresh_out_dir("cli-errors");
    let seven = dir.join("seven.pdf");
    make_seven_page_pdf(&seven);
    let o = dir.join("o.pdf");

    let out = run([OsStr::new("info"), dir.join("missing.pdf").as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).starts_with("error: cannot read"), "{}", stderr(&out));

    let junk = dir.join("junk.pdf");
    std::fs::write(&junk, b"this is not a pdf at all").expect("write");
    let out = run([OsStr::new("info"), junk.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("not a PDF file"), "{}", stderr(&out));

    for (pages, needle) in [("9", "out of range"), ("0", "start at 1"), ("3-1", "backwards"), ("x", "not a page number")] {
        let out = run([OsStr::new("delete"), seven.as_os_str(), "--pages".as_ref(), OsStr::new(pages), "-o".as_ref(), o.as_os_str()]);
        assert_eq!(code(&out), 1, "{pages}");
        assert!(stderr(&out).contains(needle), "{pages}: {}", stderr(&out));
        assert!(!o.exists(), "nothing is written when the page list is wrong");
    }
    let out = run([OsStr::new("delete"), seven.as_os_str(), "--pages".as_ref(), "1-7".as_ref(), "-o".as_ref(), o.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("every page"), "{}", stderr(&out));

    // Not an image.
    let out = run([OsStr::new("img2pdf"), junk.as_os_str(), "-o".as_ref(), o.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("junk.pdf") && stderr(&out).contains("not a JPEG or PNG"), "{}", stderr(&out));
    assert!(!o.exists());
}

/// One of the generated encrypted files, by part of its name.
fn generated(part: &str) -> common::Encrypted {
    common::generated().into_iter().find(|r| r.path.to_string_lossy().contains(part)).expect("a generated file")
}

#[test]
fn encrypted_files_need_their_password_and_say_so() {
    // PDFium's file: user password "hôtel" (the accent is why this goes through
    // the command line as text).
    let encrypted = corpus("encrypted/encrypted_hello_world_r2.pdf");
    let dir = common::fresh_out_dir("cli-encrypted");
    let o = dir.join("o.pdf");
    let seven = dir.join("seven.pdf");
    make_seven_page_pdf(&seven);
    let template = dir.join("e_%d.pdf");
    let attempts: Vec<Vec<&OsStr>> = vec![
        vec!["merge".as_ref(), seven.as_os_str(), encrypted.as_os_str(), "-o".as_ref(), o.as_os_str()],
        vec!["split".as_ref(), encrypted.as_os_str(), "--pages".as_ref(), "1".as_ref(), "-o".as_ref(), o.as_os_str()],
        vec!["delete".as_ref(), encrypted.as_os_str(), "--pages".as_ref(), "1".as_ref(), "-o".as_ref(), o.as_os_str()],
        vec!["rotate".as_ref(), encrypted.as_os_str(), "--angle".as_ref(), "90".as_ref(), "-o".as_ref(), o.as_os_str()],
        vec!["split".as_ref(), encrypted.as_os_str(), "--every".as_ref(), "1".as_ref(), "-o".as_ref(), template.as_os_str()],
        vec!["decrypt".as_ref(), encrypted.as_os_str(), "-o".as_ref(), o.as_os_str()],
    ];
    for args in &attempts {
        // No password: "needs a password", with the file's name, exit 1, nothing written.
        let out = run(args);
        assert_eq!(code(&out), 1, "{args:?}");
        let message = stderr(&out);
        assert!(message.contains("needs a password") && message.contains("encrypted_hello_world_r2.pdf"), "{message}");
        // A wrong password: "wrong password", and the password is not repeated.
        let mut wrong: Vec<&OsStr> = args.clone();
        wrong.extend([OsStr::new("--password"), OsStr::new("hunter2-not-it")]);
        let out = run(&wrong);
        assert_eq!(code(&out), 1, "{args:?}");
        let message = stderr(&out);
        assert!(message.contains("wrong password") && message.contains("encrypted_hello_world_r2.pdf"), "{message}");
        assert!(!message.contains("hunter2") && !stdout(&out).contains("hunter2"), "the password was printed: {message}");
    }
    assert!(!o.exists());
    // info works without the password: the structure is readable, the rest is said to be locked.
    let out = run([OsStr::new("info"), encrypted.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Encrypted:           yes"), "{text}");
    assert!(text.contains("Pages:               1"), "{text}");
    assert!(text.contains("Encryption:          RC4 40-bit (V1, R2)"), "{text}");
    assert!(text.contains("Password to open:    needed (none was given; use --password)"), "{text}");
    assert!(text.contains("print: no") && text.contains("fill in form fields: no"), "{text}");
    // With the password everything works and the result is still encrypted the same way.
    let with_password = |extra: &[&str]| -> Vec<OsString> {
        let mut v: Vec<OsString> = Vec::new();
        v.extend(extra.iter().map(OsString::from));
        v.extend([OsString::from("--password"), OsString::from("hôtel")]);
        v
    };
    let out = run(with_password(&["split", encrypted.to_str().unwrap(), "--pages", "1", "-o", o.to_str().unwrap()]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&run([OsStr::new("info"), o.as_os_str(), "--password".as_ref(), "hôtel".as_ref()]));
    assert!(text.contains("Encryption:          RC4 40-bit (V1, R2)") && text.contains("opened with the user password"), "{text}");
    // The file allows nothing (it is restricted): it may be the first of a merge, whose output then
    // carries its encryption, but not a later one, where the protection would be lost.
    let out = run(with_password(&["merge", seven.to_str().unwrap(), encrypted.to_str().unwrap(), "-o", o.to_str().unwrap(), "--force"]));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let message = stderr(&out);
    assert!(
        message.contains("encrypted_hello_world_r2.pdf restricts what can be done with it; put it first so its protection carries over, or give its owner password"),
        "{message}"
    );
    assert!(!message.contains("hôtel"), "{message}");
    let out = run(with_password(&["merge", encrypted.to_str().unwrap(), seven.to_str().unwrap(), "-o", o.to_str().unwrap(), "--force"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&run([OsStr::new("info"), o.as_os_str(), "--password".as_ref(), "hôtel".as_ref()]));
    assert!(text.contains("Pages:               8") && text.contains("Encryption:          RC4 40-bit (V1, R2)"), "{text}");
    // Given the owner password ("âge"), it may come second: the output is not encrypted (the first file is
    // not), and says that and that a password was needed.
    let out = run([
        OsStr::new("merge"),
        seven.as_os_str(),
        encrypted.as_os_str(),
        OsStr::new("-o"),
        o.as_os_str(),
        OsStr::new("--force"),
        OsStr::new("--password"),
        OsStr::new("âge"),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let message = stderr(&out);
    assert!(message.contains("encryption was not carried over"), "{message}");
    assert!(message.contains("it needed a password to open, and the merged file opens without one"), "{message}");
    assert!(stdout(&run([OsStr::new("info"), o.as_os_str()])).contains("Pages:               8"));
}

/// A file whose only encrypted part is the attachments says so, and a file whose permission flags were
/// edited says that.
#[test]
fn info_describes_attachments_only_encryption_and_edited_permissions() {
    let fixtures = common::workspace_root().join("crates").join("qingpdf-core").join("tests").join("encrypted_fixtures");
    let out = run([OsStr::new("info"), fixtures.join("eff-user.pdf").as_os_str(), OsStr::new("--password"), OsStr::new("u")]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Encryption:          attachments only (AES-128) (V4, R4)"), "{text}");
    assert!(!text.contains("nothing is actually encrypted"), "{text}");
    // Edit /P of a revision 6 file: five bytes, the same length.
    let original = generated("two.r6-aes256-user-everything-denied");
    let mut bytes = std::fs::read(&original.path).unwrap();
    let at = bytes.windows(8).position(|w| w == b"/P -3392").expect("the flags");
    bytes[at + 3..at + 8].copy_from_slice(b"-0004");
    let dir = common::fresh_out_dir("cli-perms");
    let edited = dir.join("edited.pdf");
    std::fs::write(&edited, &bytes).unwrap();
    let out = run([OsStr::new("info"), edited.as_os_str(), OsStr::new("--password"), OsStr::new(&original.user)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("do not agree with the check value /Perms"), "{text}");
    assert!(text.contains("print: no") && !text.contains("print: yes"), "{text}");
    // decrypt refuses with the user password, and the message says why; the owner's password does it.
    let plain = dir.join("plain.pdf");
    let out = run([OsStr::new("decrypt"), edited.as_os_str(), OsStr::new("-o"), plain.as_os_str(), OsStr::new("--password"), OsStr::new(&original.user)]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("/Perms") && stderr(&out).contains("owner password"), "{}", stderr(&out));
    assert!(!plain.exists());
    let out = run([OsStr::new("decrypt"), edited.as_os_str(), OsStr::new("-o"), plain.as_os_str(), OsStr::new("--password"), OsStr::new(&original.owner)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn the_password_works_for_chinese_and_every_command() {
    let chinese = generated("vertical.r6-aes256-user-chinese");
    let dir = common::fresh_out_dir("cli-encrypted-chinese");
    let outputs = [
        ("split", vec!["--pages", "1"]),
        ("delete", vec!["--pages", "1"]),
        ("rotate", vec!["--angle", "90"]),
    ];
    let qpdf = common::find_qpdf();
    for (i, (command, extra)) in outputs.iter().enumerate() {
        let o = dir.join(format!("{command}.pdf"));
        let mut args: Vec<OsString> = vec![OsString::from(*command), chinese.path.clone().into_os_string()];
        args.extend(extra.iter().map(OsString::from));
        // Deleting the only page of a one-page file is refused; the others work.
        args.extend([OsString::from("-o"), o.clone().into_os_string(), OsString::from("--password"), OsString::from(&chinese.user)]);
        let out = run(&args);
        if *command == "delete" {
            let pages = encrypted_page_count(&chinese.path, &chinese.user);
            if pages == 1 {
                assert_eq!(code(&out), 1);
                continue;
            }
        }
        assert_eq!(code(&out), 0, "{command}: {}", stderr(&out));
        let shown = stdout(&run([OsStr::new("info"), o.as_os_str(), OsStr::new("--password"), OsStr::new(&chinese.user)]));
        assert!(shown.contains("AES-256 (V5, R6)") && shown.contains("opened with the user password"), "{shown}");
        if let Some(qpdf) = &qpdf {
            let verdict = common::qpdf_check_pw(qpdf, &o, &chinese.user);
            assert!(verdict.code == 0 || verdict.code == 3, "{command} {i}: {}", verdict.text);
            let theirs = common::qpdf_encryption(qpdf, &o, &chinese.user).expect("qpdf opens it with the same password");
            let original = common::qpdf_encryption(qpdf, &chinese.path, &chinese.user).unwrap();
            assert_eq!((theirs.revision, theirs.permissions, &theirs.key), (original.revision, original.permissions, &original.key));
        }
    }
    // The owner password (Chinese too) opens it as the owner.
    let shown = stdout(&run([
        OsStr::new("info"),
        chinese.path.as_os_str(),
        OsStr::new("--password"),
        OsStr::new(&chinese.owner),
    ]));
    assert!(shown.contains("opened with the owner password"), "{shown}");
}

/// The page count of an encrypted file, through the library.
fn encrypted_page_count(path: &Path, password: &str) -> usize {
    qingpdf_core::Document::open_with_password(path, password).unwrap().page_count().unwrap()
}

#[test]
fn decrypt_writes_a_plain_copy_only_for_the_owner_or_when_everything_is_allowed() {
    let dir = common::fresh_out_dir("cli-decrypt");
    // Everything is allowed (only the user password is set): the user password is enough.
    let open_file = generated("bookmarks.r4-aes128-user-assemble-n");
    // ... but this one does not allow assembling.
    let o = dir.join("plain.pdf");
    let out = run([
        OsStr::new("decrypt"),
        open_file.path.as_os_str(),
        OsStr::new("-o"),
        o.as_os_str(),
        OsStr::new("--password"),
        OsStr::new(&open_file.user),
    ]);
    assert_eq!(code(&out), 1);
    let message = stderr(&out);
    assert!(message.contains("owner password") && message.contains("assemble"), "{message}");
    assert!(!o.exists());
    // The owner password does it.
    let out = run([
        OsStr::new("decrypt"),
        open_file.path.as_os_str(),
        OsStr::new("-o"),
        o.as_os_str(),
        OsStr::new("--password"),
        OsStr::new(&open_file.owner),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).starts_with("wrote "), "{}", stdout(&out));
    let text = stdout(&run([OsStr::new("info"), o.as_os_str()]));
    assert!(text.contains("Encrypted:           no") && text.contains("Pages:               2"), "{text}");
    if let Some(qpdf) = common::find_qpdf() {
        assert_eq!(common::qpdf_check(&qpdf, &o).code, 0);
    }
    // A file whose permissions allow everything: the user password is enough.
    let everything = generated("r2-40-user");
    let o2 = dir.join("plain2.pdf");
    let out = run([
        OsStr::new("decrypt"),
        everything.path.as_os_str(),
        OsStr::new("-o"),
        o2.as_os_str(),
        OsStr::new("--password"),
        OsStr::new(&everything.user),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // The output is not overwritten without --force, and not an input.
    let out = run([OsStr::new("decrypt"), everything.path.as_os_str(), OsStr::new("-o"), o2.as_os_str(), OsStr::new("--password"), OsStr::new(&everything.user)]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    let out = run([OsStr::new("decrypt"), o2.as_os_str(), OsStr::new("-o"), dir.join("again.pdf").as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("not encrypted"), "{}", stderr(&out));
}

#[test]
fn a_password_on_an_unencrypted_file_does_no_harm() {
    let dir = common::fresh_out_dir("cli-password-unneeded");
    let plain = corpus("xref-classic/hello_world_2_pages.pdf");
    let o = dir.join("o.pdf");
    let out = run([
        OsStr::new("split"),
        plain.as_os_str(),
        OsStr::new("--pages"),
        OsStr::new("1"),
        OsStr::new("-o"),
        o.as_os_str(),
        OsStr::new("--password"),
        OsStr::new("not needed"),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&run([OsStr::new("info"), o.as_os_str()])).contains("Encrypted:           no"));
}

#[test]
fn info_says_when_a_file_was_repaired() {
    let damaged = corpus("damaged/xref_command_missing.pdf");
    let out = run([OsStr::new("info"), damaged.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("Repaired:            yes"), "{}", stdout(&out));
}

#[test]
fn merge_says_what_it_could_not_carry_over() {
    let with_bookmarks = corpus("outline-form-attach/bookmarks.pdf");
    let with_form = corpus("outline-form-attach/multiple_form_types.pdf");
    let dir = common::fresh_out_dir("cli-merge-warnings");
    let o = dir.join("o.pdf");
    // The second file's form is not carried over: a warning on stderr, exit 0.
    let out = run([OsStr::new("merge"), with_bookmarks.as_os_str(), with_form.as_os_str(), "-o".as_ref(), o.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let warning = stderr(&out);
    assert!(warning.starts_with("warning: ") && warning.contains("multiple_form_types.pdf") && warning.contains("form fields"), "{warning}");
    assert!(!warning.contains("bookmarks.pdf:"), "the base file loses nothing: {warning}");
    // The other way round it is the bookmarks that are left behind.
    let o2 = dir.join("o2.pdf");
    let out = run([OsStr::new("merge"), with_form.as_os_str(), with_bookmarks.as_os_str(), "-o".as_ref(), o2.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stderr(&out).contains("bookmarks"), "{}", stderr(&out));
}

#[test]
fn img2pdf_page_modes_and_png() {
    let dir = common::fresh_out_dir("cli-img2pdf");
    // A PNG with transparency, written here.
    let png_path = dir.join("alpha.png");
    {
        let file = std::fs::File::create(&png_path).expect("create");
        let mut enc = png::Encoder::new(file, 20, 10);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("header");
        let data: Vec<u8> = (0..200u32).flat_map(|i| [i as u8, 100, 200, (i * 5 % 256) as u8]).collect();
        w.write_image_data(&data).expect("data");
    }
    // --page fit: 20x10 pixels at 96 dpi is 15 x 7.5 pt.
    let fit = dir.join("fit.pdf");
    let out = run([OsStr::new("img2pdf"), png_path.as_os_str(), "--page".as_ref(), "fit".as_ref(), "-o".as_ref(), fit.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&run([OsStr::new("info"), fit.as_os_str()]));
    assert!(text.contains("15 x 7.5 pt"), "{text}");
    // Default: A4 landscape for a wide image.
    let a4 = dir.join("a4.pdf");
    let out = run([OsStr::new("img2pdf"), png_path.as_os_str(), "-o".as_ref(), a4.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&run([OsStr::new("info"), a4.as_os_str()])).contains("841.89 x 595.28 pt"));
    // A bad mode is a usage error.
    let out = run([OsStr::new("img2pdf"), png_path.as_os_str(), "--page".as_ref(), "letter".as_ref(), "-o".as_ref(), dir.join("x.pdf").as_os_str()]);
    assert_eq!(code(&out), 2);
}

#[test]
fn text_prints_or_writes_the_text_of_the_chosen_pages() {
    let dir = common::fresh_out_dir("cli-text");
    let handmade = common::corpus_root().join("public").join("zh").join("handmade");
    let expected = std::fs::read_to_string(handmade.join("zh-gb1-h-tounicode-differs.expected.txt")).expect("expected text");
    let pdf = handmade.join("zh-gb1-h-tounicode-differs.pdf");

    // to the screen: UTF-8, exactly the expected lines
    let out = run([OsStr::new("text"), pdf.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), expected);

    // to a file; the file is never overwritten without --force
    let target = dir.join("out.txt");
    let out = run([OsStr::new("text"), pdf.as_os_str(), "-o".as_ref(), target.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("wrote") && stdout(&out).contains("(1 page)"), "{}", stdout(&out));
    assert_eq!(std::fs::read(&target).expect("written"), expected.as_bytes());
    let out = run([OsStr::new("text"), pdf.as_os_str(), "-o".as_ref(), target.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("--force"), "{}", stderr(&out));
    let out = run([OsStr::new("text"), pdf.as_os_str(), "-o".as_ref(), target.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // pages are separated by a form feed; --pages picks and orders them
    let two = corpus("xref-classic/hello_world_2_pages.pdf");
    let out = run([OsStr::new("text"), two.as_os_str()]);
    assert_eq!(stdout(&out), "Hello, world!\nGoodbye, world!\n\u{c}Hello, world!\nGoodbye, world!\n");
    let out = run([OsStr::new("text"), two.as_os_str(), "--pages".as_ref(), "2".as_ref()]);
    assert_eq!(stdout(&out), "Hello, world!\nGoodbye, world!\n");
    let out = run([OsStr::new("text"), two.as_os_str(), "--pages".as_ref(), "3".as_ref()]);
    assert_eq!(code(&out), 1, "a page that does not exist is an error");

    // usage errors, and the help
    assert_eq!(code(&run([OsStr::new("text"), two.as_os_str(), "--force".as_ref()])), 2);
    assert_eq!(code(&run(["text"])), 2);
    assert_eq!(code(&run([OsStr::new("text"), two.as_os_str(), "--angle".as_ref(), "90".as_ref()])), 2);
    let help = stdout(&run(["text", "--help"]));
    assert!(help.contains("qingpdf text") && help.contains("--pages") && help.contains("owner password"), "{help}");
    assert!(stdout(&run(["--help"])).contains("text"));
}

#[test]
fn render_writes_one_png_per_page() {
    let dir = common::fresh_out_dir("cli-render");
    let two = corpus("xref-classic/hello_world_2_pages.pdf");
    let pattern = dir.join("page_%d.png");

    let out = run([OsStr::new("render"), two.as_os_str(), "--dpi".as_ref(), "36".as_ref(), "-o".as_ref(), pattern.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("page_1.png") && stdout(&out).contains("page_2.png"), "{}", stdout(&out));
    // The letters of the page are drawn from system fonts (or boxes, with a word to the user, where there are none).
    assert!(!stderr(&out).contains("error"), "{}", stderr(&out));
    for n in 1..=2 {
        let png = std::fs::read(dir.join(format!("page_{n}.png"))).expect("written");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let reader = decoder.read_info().expect("a PNG");
        // 200 by 200 points at 36 dpi
        assert_eq!((reader.info().width, reader.info().height), (100, 100));
    }

    // Not over existing files without --force; --pages picks; the name needs %d for several pages.
    let out = run([OsStr::new("render"), two.as_os_str(), "-o".as_ref(), pattern.as_os_str(), "--pages".as_ref(), "2".as_ref(), "--dpi".as_ref(), "36".as_ref()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("--force"), "{}", stderr(&out));
    let single = dir.join("only.png");
    let out = run([OsStr::new("render"), two.as_os_str(), "-o".as_ref(), single.as_os_str(), "--pages".as_ref(), "2".as_ref(), "--dpi".as_ref(), "36".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(single.is_file());
    let out = run([OsStr::new("render"), two.as_os_str(), "-o".as_ref(), single.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("%d"), "{}", stderr(&out));
    let out = run([OsStr::new("render"), two.as_os_str(), "-o".as_ref(), pattern.as_os_str(), "--pages".as_ref(), "3".as_ref(), "--force".as_ref()]);
    assert_eq!(code(&out), 1, "a page that does not exist is an error");

    // A page that would be too big is an error naming the way out; usage errors; the help.
    let out = run([OsStr::new("render"), two.as_os_str(), "-o".as_ref(), pattern.as_os_str(), "--dpi".as_ref(), "2400".as_ref(), "--force".as_ref()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("--dpi"), "{}", stderr(&out));
    // When a later page fails, the pages before it were written and are reported (and page 2 is named).
    let half = dir.join("half.pdf");
    std::fs::write(
        &half,
        b"%PDF-1.4
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R 4 0 R]/Count 2>>endobj
          3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]>>endobj
4 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 100000 100000]>>endobj
          trailer<</Root 1 0 R/Size 5>>
",
    )
    .expect("written");
    let out = run([OsStr::new("render"), half.as_os_str(), "-o".as_ref(), pattern.as_os_str(), "--force".as_ref()]);
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("page_1.png") && !stdout(&out).contains("page_2.png"), "{}", stdout(&out));
    assert!(stderr(&out).contains("page 2"), "{}", stderr(&out));
    assert_eq!(code(&run([OsStr::new("render"), two.as_os_str(), "--dpi".as_ref(), "0".as_ref(), "-o".as_ref(), pattern.as_os_str()])), 2);
    assert_eq!(code(&run([OsStr::new("render"), two.as_os_str(), "--dpi".as_ref(), "abc".as_ref(), "-o".as_ref(), pattern.as_os_str()])), 2);
    assert_eq!(code(&run([OsStr::new("render"), two.as_os_str()])), 2);
    let help = stdout(&run(["render", "--help"]));
    assert!(help.contains("qingpdf render") && help.contains("--dpi") && help.contains("%d"), "{help}");
    assert!(stdout(&run(["--help"])).contains("render"));
}

#[test]
fn text_obeys_the_copy_permission_and_the_password() {
    let dir = common::generated_dir();
    // copying not allowed, empty user password: refused, and the message says what to do
    let restricted = dir.join("two.r2-40-empty-modify-extract-none.pdf");
    let out = run([OsStr::new("text"), restricted.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("owner password") && stderr(&out).contains("--password"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    // the owner password gives the text
    let out = run([OsStr::new("text"), restricted.as_os_str(), "--password".as_ref(), "owner".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("Hello, world!"), "{}", stdout(&out));
    // a file that needs its password
    let locked = dir.join("bookmarks.r2-40-user.pdf");
    let out = run([OsStr::new("text"), locked.as_os_str()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("needs a password"), "{}", stderr(&out));
    let out = run([OsStr::new("text"), locked.as_os_str(), "--password".as_ref(), "user".as_ref()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = run([OsStr::new("text"), locked.as_os_str(), "--password".as_ref(), "wrong".as_ref()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("wrong password"), "{}", stderr(&out));
}

#[test]
fn text_reports_what_it_skipped_and_never_prints_escape_sequences() {
    // a content stream in a filter we do not read (DCTDecode): a warning on stderr, no text, success
    let dir = common::fresh_out_dir("cli-text-unreadable");
    let pdf = dir.join("dct-content.pdf");
    let mut file = b"%PDF-1.4
".to_vec();
    let mut offsets = Vec::new();
    let bodies: [&[u8]; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
        b"<< /Length 2 /Filter /DCTDecode >>
stream
xx
endstream",
    ];
    for (i, body) in bodies.iter().enumerate() {
        offsets.push(file.len());
        file.extend_from_slice(format!("{} 0 obj
", i + 1).as_bytes());
        file.extend_from_slice(body);
        file.extend_from_slice(b"
endobj
");
    }
    let xref = file.len();
    file.extend_from_slice(b"xref
0 5
0000000000 65535 f 
");
    for off in &offsets {
        file.extend_from_slice(format!("{off:010} 00000 n 
").as_bytes());
    }
    file.extend_from_slice(format!("trailer
<< /Size 5 /Root 1 0 R >>
startxref
{xref}
%%EOF
").as_bytes());
    std::fs::write(&pdf, file).expect("write the test file");
    let out = run([OsStr::new("text"), pdf.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stderr(&out).starts_with("warning:") && stderr(&out).contains("skipped"), "{}", stderr(&out));
    // text taken from files never carries control characters to the terminal
    for rel in ["cjk/90ms_rksj_h_sample.pdf", "pdf20/pdf20-utf8-test.pdf", "outline-form-attach/bookmarks.pdf"] {
        let out = run([OsStr::new("text"), corpus(rel).as_os_str()]);
        assert_eq!(code(&out), 0, "{rel}: {}", stderr(&out));
        assert!(!stdout(&out).chars().any(|c| c.is_control() && c != '\n' && c != '\u{c}'), "{rel}");
    }
}
