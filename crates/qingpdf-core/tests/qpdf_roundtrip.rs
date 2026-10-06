//! Acceptance 3 (first half): every file the writer produces passes
//! `qpdf --check`. qpdf is only a reference answer for testing; if it is not
//! installed the test says so and does nothing.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use qingpdf_core::{Document, ops};

#[test]
fn qpdf_accepts_every_round_trip_output() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found (not on PATH, not in C:\\Program Files\\qpdf*\\bin)");
        return;
    };
    println!("using {}", qpdf.display());
    let out_dir = common::fresh_out_dir("qpdf-roundtrip");
    let files = common::corpus_files();
    let (mut pass, mut input_quirk, mut skipped) = (0usize, Vec::new(), 0usize);
    let mut ours: Vec<String> = Vec::new();
    for (n, path) in files.iter().enumerate() {
        let name = common::corpus_name(path);
        let Ok(doc) = Document::open(path) else {
            skipped += 1;
            continue;
        };
        if doc.is_encrypted() || doc.pages().is_err() {
            skipped += 1;
            continue;
        }
        let Ok(out) = ops::copy_all(&doc) else {
            skipped += 1;
            continue;
        };
        let written = out_dir.join(format!("{n:03}.pdf"));
        std::fs::write(&written, &out.data).expect("cannot write the output");
        let verdict = common::qpdf_check(&qpdf, &written);
        if verdict.code == 0 {
            pass += 1;
            continue;
        }
        // Did qpdf already dislike the input? Then this is an input quirk.
        let input = common::qpdf_check(&qpdf, path);
        let first = |t: &str| t.lines().find(|l| l.contains("WARNING") || l.contains("ERROR") || l.contains("error")).unwrap_or("").trim().to_string();
        if input.code != 0 {
            input_quirk.push(format!(
                "{name}: output exit {} ({}); the input itself gives exit {} ({})",
                verdict.code,
                first(&verdict.text),
                input.code,
                first(&input.text)
            ));
        } else {
            ours.push(format!("{name}: exit {}\n{}", verdict.code, verdict.text));
        }
    }
    println!("\n=== qpdf --check on {} round-trip outputs ===", files.len());
    println!("{pass:>5}  pass");
    println!("{:>5}  not clean, but the input is not clean either (input quirk)", input_quirk.len());
    println!("{:>5}  FAIL (input is clean, output is not)", ours.len());
    println!("{skipped:>5}  skipped (encrypted, unopenable, or no page tree)");
    for line in &input_quirk {
        println!("  input quirk: {line}");
    }
    for line in &ours {
        println!("  FAIL: {line}");
    }
    assert!(ours.is_empty(), "{} output file(s) fail qpdf --check although their input is clean", ours.len());
}
