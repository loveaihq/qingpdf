//! Acceptance 3 (first half), for the commands: whatever `merge`, `split`,
//! `delete`, `rotate` and `img2pdf` write passes `qpdf --check`. Skipped, with
//! a note, when qpdf is not installed.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use qingpdf_core::Document;

fn qingpdf(args: &[OsString]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_qingpdf")).args(args).output().expect("cannot run qingpdf");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn args(list: &[&str], files: &[&Path]) -> Vec<OsString> {
    // Words first, then the files in the order they are used (marked by "@").
    let mut files = files.iter();
    list.iter()
        .map(|w| if *w == "@" { files.next().map_or_else(OsString::new, |f| f.as_os_str().to_os_string()) } else { OsString::from(w) })
        .collect()
}

fn fixtures() -> PathBuf {
    common::workspace_root().join("crates").join("qingpdf-core").join("tests").join("fixtures")
}

/// Corpus files our reader opens, unencrypted, with at least one page.
fn usable_corpus() -> Vec<(PathBuf, usize)> {
    common::corpus_files()
        .into_iter()
        .filter_map(|p| {
            let doc = Document::open(&p).ok()?;
            if doc.is_encrypted() {
                return None;
            }
            let n = doc.pages().ok()?.len();
            (n > 0).then_some((p, n))
        })
        .collect()
}

/// qpdf warnings that come from the inputs or from a documented limit, not from
/// the writer: damaged stream data and an unsorted name tree are copied from the
/// input as they are; a merged-in file's form widgets are not reachable from the
/// base file's /AcroForm because form fields are not merged (the user is warned).
const EXPLAINED: [&str; 6] = [
    "error decoding stream data",
    "stream will be re-processed without filtering",
    "input stream is complete but output may still be valid",
    "keys are not sorted",
    "attempting to repair after error",
    "widget annotation is not reachable from /AcroForm",
];

struct Tally {
    pass: usize,
    /// The input already gives the same kind of trouble.
    input_quirk: Vec<String>,
    /// Warnings explained by a documented limit or by damaged input data (merges).
    explained: Vec<String>,
    failures: Vec<String>,
}

impl Tally {
    /// Check `file`, which was made from `inputs`.
    fn check(&mut self, qpdf: &Path, label: &str, file: &Path, inputs: &[&Path]) {
        let verdict = common::qpdf_check(qpdf, file);
        if verdict.code == 0 {
            self.pass += 1;
            return;
        }
        // One input that qpdf already dislikes: whatever it says about the output is an input quirk.
        if let [input] = inputs
            && common::qpdf_check(qpdf, input).code != 0
            && verdict.code == 3
        {
            self.input_quirk.push(label.to_string());
            return;
        }
        let problems: Vec<&str> =
            verdict.text.lines().filter(|l| l.starts_with("WARNING") || l.starts_with("ERROR")).collect();
        if verdict.code == 3 && !problems.is_empty() && problems.iter().all(|l| EXPLAINED.iter().any(|e| l.contains(e))) {
            self.explained.push(format!("{label}: {}", problems.first().copied().unwrap_or("")));
            return;
        }
        self.failures.push(format!("{label}: exit {}\n{}", verdict.code, verdict.text));
    }
}

#[test]
fn qpdf_accepts_the_output_of_every_command() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found (not on PATH, not in C:\\Program Files\\qpdf*\\bin)");
        return;
    };
    println!("using {}", qpdf.display());
    let dir = common::fresh_out_dir("qpdf-commands");
    let usable = usable_corpus();
    assert!(!usable.is_empty(), "no corpus files to work with");
    let mut tally = Tally { pass: 0, input_quirk: Vec::new(), explained: Vec::new(), failures: Vec::new() };
    let mut commands_run = 0usize;

    // merge: the whole usable corpus in one go, and pairs of neighbours.
    let all: Vec<&Path> = usable.iter().map(|(p, _)| p.as_path()).collect();
    let big = dir.join("merge-all.pdf");
    let mut a = vec![OsString::from("merge")];
    a.extend(all.iter().map(|p| p.as_os_str().to_os_string()));
    a.extend([OsString::from("-o"), big.clone().into_os_string()]);
    let (code, err) = qingpdf(&a);
    assert_eq!(code, 0, "merge of everything failed: {err}");
    commands_run += 1;
    tally.check(&qpdf, "merge of all usable corpus files", &big, &all);
    // Pairs and per-file runs use the files up to 1.5 MB (qpdf is slow on big ones).
    let small: Vec<(PathBuf, usize)> = usable
        .iter()
        .filter(|(p, _)| std::fs::metadata(p).map(|m| m.len() <= 1_500_000).unwrap_or(false))
        .cloned()
        .collect();
    for (i, pair) in small.windows(2).enumerate().step_by(4) {
        let out = dir.join(format!("merge-pair-{i}.pdf"));
        let a = args(&["merge", "@", "@", "-o", "@"], &[&pair[0].0, &pair[1].0, &out]);
        let (code, err) = qingpdf(&a);
        assert_eq!(code, 0, "merge {:?}: {err}", pair.iter().map(|p| common::corpus_name(&p.0)).collect::<Vec<_>>());
        commands_run += 1;
        tally.check(
            &qpdf,
            &format!("merge {} + {}", common::corpus_name(&pair[0].0), common::corpus_name(&pair[1].0)),
            &out,
            &[&pair[0].0, &pair[1].0],
        );
    }

    // split / delete / rotate on files with several pages, and on a sample of others.
    let mut multi: Vec<&(PathBuf, usize)> = small.iter().filter(|(_, n)| *n >= 3).collect();
    let single: Vec<&(PathBuf, usize)> = small.iter().filter(|(_, n)| *n < 3).step_by(4).collect();
    multi.extend(single);
    for (i, (path, pages)) in multi.into_iter().enumerate() {
        let name = common::corpus_name(path);
        let mut run = |what: &str, a: Vec<OsString>, out: PathBuf| {
            let (code, err) = qingpdf(&a);
            assert_eq!(code, 0, "{what} on {name} failed: {err}");
            commands_run += 1;
            tally.check(&qpdf, &format!("{what} on {name}"), &out, &[path]);
        };
        let out = dir.join(format!("split-{i}.pdf"));
        let list = if *pages >= 2 { "1,2-" } else { "1" };
        run("split --pages", args(&["split", "@", "--pages", list, "-o", "@"], &[path, &out]), out.clone());
        let out = dir.join(format!("last-{i}.pdf"));
        run("split --pages (last page)", args(&["split", "@", "--pages", &format!("{pages}"), "-o", "@"], &[path, &out]), out.clone());
        if *pages >= 2 {
            let out = dir.join(format!("delete-{i}.pdf"));
            run("delete", args(&["delete", "@", "--pages", "1", "-o", "@"], &[path, &out]), out.clone());
        }
        let out = dir.join(format!("rotate-{i}.pdf"));
        run("rotate", args(&["rotate", "@", "--angle", "90", "-o", "@"], &[path, &out]), out.clone());
        // split --every: about three files, each one checked.
        let every = (*pages / 3).max(1);
        let template = dir.join(format!("every-{i}-%d.pdf"));
        let a = args(&["split", "@", "--every", &every.to_string(), "-o", "@"], &[path, &template]);
        let (code, err) = qingpdf(&a);
        assert_eq!(code, 0, "split --every on {name}: {err}");
        commands_run += 1;
        for n in 1..=pages.div_ceil(every) {
            let piece = dir.join(format!("every-{i}-{n}.pdf"));
            tally.check(&qpdf, &format!("split --every {every} on {name}, file {n}"), &piece, &[path]);
        }
    }

    // img2pdf: every fixture JPEG, a PNG with and without alpha, both page modes.
    let png_alpha = dir.join("alpha.png");
    let png_plain = dir.join("plain.png");
    for (path, color) in [(&png_alpha, png::ColorType::Rgba), (&png_plain, png::ColorType::Rgb)] {
        let channels = if color == png::ColorType::Rgba { 4 } else { 3 };
        let mut enc = png::Encoder::new(std::fs::File::create(path).expect("create"), 33, 21);
        enc.set_color(color);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("header");
        let data: Vec<u8> = (0..33 * 21 * channels).map(|i| (i * 7 % 251) as u8).collect();
        w.write_image_data(&data).expect("data");
    }
    let mut images: Vec<PathBuf> = std::fs::read_dir(fixtures()).expect("fixtures").flatten().map(|e| e.path()).collect();
    images.sort();
    images.push(png_alpha);
    images.push(png_plain);
    for mode in ["a4", "fit"] {
        let out = dir.join(format!("img2pdf-{mode}.pdf"));
        let mut a = vec![OsString::from("img2pdf")];
        a.extend(images.iter().map(|p| p.as_os_str().to_os_string()));
        a.extend(["--page".into(), mode.into(), "-o".into(), out.clone().into_os_string()]);
        let (code, err) = qingpdf(&a);
        assert_eq!(code, 0, "img2pdf --page {mode}: {err}");
        commands_run += 1;
        let refs: Vec<&Path> = images.iter().map(PathBuf::as_path).collect();
        tally.check(&qpdf, &format!("img2pdf --page {mode} ({} images)", images.len()), &out, &refs);
    }
    for (i, image) in images.iter().enumerate() {
        let out = dir.join(format!("img-{i}.pdf"));
        let (code, err) = qingpdf(&args(&["img2pdf", "@", "-o", "@"], &[image, &out]));
        assert_eq!(code, 0, "img2pdf {}: {err}", image.display());
        commands_run += 1;
        tally.check(&qpdf, &format!("img2pdf {}", image.display()), &out, &[]);
    }

    println!("\n=== qpdf --check on command outputs: {commands_run} commands ===");
    println!("{:>5}  output files pass", tally.pass);
    println!("{:>5}  not clean, but the (single) input is not clean either: input quirk", tally.input_quirk.len());
    println!("{:>5}  merges with warnings explained by damaged input data or by form fields not being merged", tally.explained.len());
    println!("{:>5}  output files FAIL", tally.failures.len());
    for q in &tally.input_quirk {
        println!("  input quirk: {q}");
    }
    for e in &tally.explained {
        println!("  explained: {e}");
    }
    for f in &tally.failures {
        println!("  FAIL: {f}");
    }
    assert!(tally.failures.is_empty(), "{} output file(s) fail qpdf --check", tally.failures.len());
}
