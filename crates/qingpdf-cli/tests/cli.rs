//! The `qingpdf` program, run as a user would: arguments in, files and exit
//! codes out. Inputs are made with `img2pdf` from the JPEG fixtures, or taken
//! from the public corpus.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;

use std::ffi::OsStr;
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

fn corpus(rel: &str) -> PathBuf {
    common::corpus_root().join("public").join(rel)
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
    for word in ["info", "merge", "split", "delete", "rotate", "img2pdf", "--force"] {
        assert!(text.contains(word), "main help should mention {word}");
    }
    for command in ["info", "merge", "split", "delete", "rotate", "img2pdf"] {
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

#[test]
fn encrypted_files_are_refused_clearly_but_info_still_reports() {
    let encrypted = corpus("encrypted/encrypted_hello_world_r2.pdf");
    if !encrypted.exists() {
        println!("SKIPPED: corpus file not found");
        return;
    }
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
    ];
    for args in attempts {
        let out = run(&args);
        assert_eq!(code(&out), 1, "{args:?}");
        let message = stderr(&out);
        assert!(message.contains("is encrypted") && message.contains("not supported yet"), "{message}");
        assert!(message.contains("encrypted_hello_world_r2.pdf"), "{message}");
    }
    assert!(!o.exists());
    // info works: structure is readable.
    let out = run([OsStr::new("info"), encrypted.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Encrypted:           yes"), "{text}");
    assert!(text.contains("Pages:               1"), "{text}");
}

#[test]
fn info_says_when_a_file_was_repaired() {
    let damaged = corpus("damaged/xref_command_missing.pdf");
    if !damaged.exists() {
        println!("SKIPPED: corpus file not found");
        return;
    }
    let out = run([OsStr::new("info"), damaged.as_os_str()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("Repaired:            yes"), "{}", stdout(&out));
}

#[test]
fn merge_says_what_it_could_not_carry_over() {
    let with_bookmarks = corpus("outline-form-attach/bookmarks.pdf");
    let with_form = corpus("outline-form-attach/multiple_form_types.pdf");
    if !with_bookmarks.exists() || !with_form.exists() {
        println!("SKIPPED: corpus files not found");
        return;
    }
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
