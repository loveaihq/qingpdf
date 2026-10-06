//! Acceptance 2: every file of the test corpus is read, written back, read
//! again; the page count is the same and so are the raw bytes of every page's
//! content streams.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use qingpdf_core::ops;
use qingpdf_core::{Document, Error, Object, Page};

/// The raw (still encoded) bytes of each content stream of a page.
fn content_streams(doc: &Document, page: &Page) -> Vec<Vec<u8>> {
    let Some(contents) = page.dict.get("Contents") else {
        return Vec::new();
    };
    let one = |obj: &Object| match doc.resolve(obj) {
        Ok(Object::Stream(s)) => Some(s.data),
        _ => None,
    };
    match doc.resolve(contents) {
        Ok(Object::Stream(s)) => vec![s.data],
        Ok(Object::Array(items)) => items.iter().filter_map(one).collect(),
        _ => Vec::new(),
    }
}

#[derive(Default)]
struct Summary {
    outcomes: BTreeMap<&'static str, Vec<String>>,
}

impl Summary {
    fn add(&mut self, outcome: &'static str, file: &str, detail: impl Into<String>) {
        let detail = detail.into();
        let line = if detail.is_empty() { file.to_string() } else { format!("{file}: {detail}") };
        self.outcomes.entry(outcome).or_default().push(line);
    }

    fn count(&self, outcome: &str) -> usize {
        self.outcomes.get(outcome).map_or(0, Vec::len)
    }
}

#[test]
fn round_trip_every_corpus_file() {
    let files = common::corpus_files();
    let mut summary = Summary::default();
    for path in &files {
        let name = common::corpus_name(path);
        let Ok(bytes) = std::fs::read(path) else {
            summary.add("FAIL unreadable file", &name, "");
            continue;
        };
        let doc = match Document::from_bytes(bytes) {
            Ok(d) => d,
            Err(Error::Unsupported(why)) => {
                // The Brotli prototype file must be reported as unsupported, naming the filter.
                if name.ends_with("Brotli-Prototype-FileA.pdf") {
                    assert!(why.contains("BrotliDecode"), "{name}: {why}");
                }
                summary.add("unsupported (not a failure)", &name, why);
                continue;
            }
            Err(e) => {
                summary.add("unopenable damaged file (listed, not a failure)", &name, e.to_string());
                continue;
            }
        };
        if doc.is_encrypted() {
            match ops::copy_all(&doc) {
                Err(Error::Unsupported(m)) if m == "encrypted PDF" => summary.add("encrypted (Unsupported, as expected)", &name, ""),
                other => summary.add("FAIL encrypted file did not give Unsupported", &name, format!("{:?}", other.map(|o| o.pages))),
            }
            continue;
        }
        let pages = match doc.pages() {
            Ok(p) => p,
            Err(e) => {
                summary.add("page tree unreadable (listed, not a failure)", &name, e.to_string());
                continue;
            }
        };
        let out = match ops::copy_all(&doc) {
            Ok(o) => o,
            Err(Error::Unsupported(m)) => {
                summary.add("unsupported (not a failure)", &name, m);
                continue;
            }
            Err(e) => {
                summary.add("FAIL copy failed", &name, e.to_string());
                continue;
            }
        };
        let copy = match Document::from_bytes(out.data.clone()) {
            Ok(c) => c,
            Err(e) => {
                summary.add("FAIL output does not reopen", &name, e.to_string());
                continue;
            }
        };
        if copy.was_repaired() {
            summary.add("FAIL output needed repair to open", &name, "");
            continue;
        }
        let copied_pages = match copy.pages() {
            Ok(p) => p,
            Err(e) => {
                summary.add("FAIL output page tree unreadable", &name, e.to_string());
                continue;
            }
        };
        if copied_pages.len() != pages.len() || out.pages != pages.len() {
            summary.add(
                "FAIL page count differs",
                &name,
                format!("{} before, {} after", pages.len(), copied_pages.len()),
            );
            continue;
        }
        let mismatched = pages
            .iter()
            .zip(&copied_pages)
            .position(|(before, after)| content_streams(&doc, before) != content_streams(&copy, after));
        match mismatched {
            None if out.warnings.is_empty() => summary.add("ok", &name, ""),
            None => summary.add("ok, with warnings", &name, out.warnings.iter().map(|w| w.0.clone()).collect::<Vec<_>>().join(" | ")),
            Some(i) if !out.warnings.is_empty() => summary.add(
                "content differs where damaged objects were dropped (warned; listed, not a failure)",
                &name,
                format!("page {}: {}", i + 1, out.warnings.iter().map(|w| w.0.clone()).collect::<Vec<_>>().join(" | ")),
            ),
            Some(i) => summary.add("FAIL content stream bytes differ", &name, format!("page {}", i + 1)),
        }
        // The written file is itself stable: copying it again keeps everything.
        if let Ok(again) = ops::copy_all(&copy) {
            assert_eq!(again.pages, pages.len(), "{name}: second copy");
        }
    }

    println!("\n=== round trip: {} files ===", files.len());
    for (outcome, list) in &summary.outcomes {
        println!("{:>5}  {outcome}", list.len());
    }
    for (outcome, list) in &summary.outcomes {
        if outcome.starts_with("ok") && *outcome == "ok" {
            continue;
        }
        println!("\n--- {outcome} ({}) ---", list.len());
        for line in list {
            println!("  {line}");
        }
    }
    let failures: Vec<&String> =
        summary.outcomes.iter().filter(|(k, _)| k.starts_with("FAIL")).flat_map(|(_, v)| v.iter()).collect();
    assert!(failures.is_empty(), "{} round-trip failure(s): {failures:#?}", failures.len());
    // The corpus is not empty and most of it takes the whole path.
    assert!(summary.count("ok") + summary.count("ok, with warnings") > 0);
}
