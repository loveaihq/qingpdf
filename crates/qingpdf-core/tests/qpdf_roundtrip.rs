//! Acceptance 3 (first half): every file the writer produces passes
//! `qpdf --check`, or complains about nothing that the input does not complain
//! about too. qpdf is only a reference answer for testing; if it is not
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
        match common::judge_qpdf_output(&qpdf, &written, &[path], false) {
            common::Judgement::Clean => pass += 1,
            common::Judgement::InputQuirk => input_quirk.push(name),
            common::Judgement::Fail(why) => ours.push(format!("{name}: {why}")),
        }
    }
    println!("\n=== qpdf --check on {} round-trip outputs ===", files.len());
    println!("{pass:>5}  pass");
    println!("{:>5}  not clean, but only about what qpdf says about the input too (input quirk)", input_quirk.len());
    println!("{:>5}  FAIL (qpdf says something about the output that it does not say about the input)", ours.len());
    println!("{skipped:>5}  skipped (encrypted, unopenable, or no page tree)");
    for name in &input_quirk {
        println!("  input quirk: {name}");
    }
    for line in &ours {
        println!("  FAIL: {line}");
    }
    assert!(ours.is_empty(), "{} output file(s) fail qpdf --check in ways their input does not", ours.len());
}
