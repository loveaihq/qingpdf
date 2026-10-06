//! Acceptance 4: damaged files. From every public corpus file a generator
//! makes truncated copies, copies with a wrong `startxref`, copies with the
//! cross-reference data scrambled, and copies with random bytes changed. Each
//! one must either open or give an error, within 5 seconds, without a panic;
//! the ones that open are also walked (pages, info) and copied, and what the
//! copy writes must open again.
//!
//! Set `QINGPDF_WRITE_BROKEN=1` to also write the generated files to
//! `tests/corpus/broken/` (git-ignored) for use with other tools.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use qingpdf_core::info;
use qingpdf_core::{Document, Error, ops};

const TIME_LIMIT: Duration = Duration::from_secs(5);

/// What happened to one damaged file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Refused with an error of this kind.
    Refused(&'static str),
    /// Opened; the copy was written and reopens.
    OpenedAndCopied,
    /// Opened; the copy was refused with an error of this kind.
    OpenedCopyRefused(&'static str),
    /// Opened, copied, and the copy does not open: a writer bug.
    CopyDoesNotReopen(String),
    Panicked(String),
}

fn kind(e: &Error) -> &'static str {
    match e {
        Error::Io(_) => "io",
        Error::Syntax { .. } => "syntax",
        Error::MissingObject { .. } => "missing object",
        Error::Unsupported(_) => "unsupported",
        Error::Limit(_) => "limit",
        Error::Invalid(_) => "invalid",
    }
}

fn exercise(bytes: Vec<u8>) -> Outcome {
    let run = catch_unwind(AssertUnwindSafe(|| match Document::from_bytes(bytes) {
        Err(e) => Outcome::Refused(kind(&e)),
        Ok(doc) => {
            let _ = doc.version();
            let _ = doc.page_count();
            let _ = doc.pages();
            let _ = doc.info();
            let _ = info::describe(&doc);
            match ops::copy_all(&doc) {
                Err(e) => Outcome::OpenedCopyRefused(kind(&e)),
                Ok(out) => match Document::from_bytes(out.data) {
                    Ok(copy) if !copy.was_repaired() => {
                        let _ = copy.pages();
                        Outcome::OpenedAndCopied
                    }
                    Ok(_) => Outcome::CopyDoesNotReopen("the copy needed repair".to_string()),
                    Err(e) => Outcome::CopyDoesNotReopen(e.to_string()),
                },
            }
        }
    }));
    match run {
        Ok(outcome) => outcome,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_string());
            Outcome::Panicked(message)
        }
    }
}

/// Run `exercise` on its own thread and give up waiting after the limit.
fn exercise_with_limit(bytes: Vec<u8>) -> (Option<Outcome>, Duration) {
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(exercise(bytes));
    });
    match rx.recv_timeout(TIME_LIMIT) {
        Ok(outcome) => (Some(outcome), started.elapsed()),
        Err(_) => (None, started.elapsed()),
    }
}

// --- the generator ----------------------------------------------------------------------

/// Where the last occurrence of `needle` starts.
fn find_last(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).rposition(|w| w == needle)
}

struct Variant {
    family: &'static str,
    label: String,
    data: Vec<u8>,
}

/// The number after the last `startxref`, with where its digits are.
fn startxref_value(data: &[u8]) -> Option<(usize, usize, u64)> {
    let at = find_last(data, b"startxref")? + b"startxref".len();
    let start = at + data.get(at..)?.iter().position(|b| b.is_ascii_digit())?;
    let len = data.get(start..)?.iter().take_while(|b| b.is_ascii_digit()).count();
    let value = std::str::from_utf8(data.get(start..start + len)?).ok()?.parse::<u64>().ok()?;
    Some((start, start + len, value))
}

fn variants(data: &[u8], rng: &mut common::XorShift) -> Vec<Variant> {
    let len = data.len();
    let mut out = Vec::new();

    // 1. Truncations at several points, and a few random ones.
    let mut cuts = vec![0, 1, 4, 9, 20, len / 10, len / 4, len / 3, len / 2, len * 2 / 3, len * 3 / 4, len * 9 / 10];
    cuts.extend([len.saturating_sub(100), len.saturating_sub(30), len.saturating_sub(10), len.saturating_sub(1)]);
    for _ in 0..3 {
        cuts.push(rng.below(len.max(1)));
    }
    cuts.sort_unstable();
    cuts.dedup();
    for cut in cuts.into_iter().filter(|&c| c < len) {
        out.push(Variant { family: "truncated", label: format!("cut{cut}"), data: data.get(..cut).unwrap_or(&[]).to_vec() });
    }

    // 2. The startxref offset changed.
    if let Some((from, to, value)) = startxref_value(data) {
        let len64 = len as u64;
        let mut replacements = vec![
            value.saturating_add(1),
            value.saturating_sub(1),
            value.saturating_add(17),
            0,
            len64.saturating_add(1000),
            9_999_999_999,
            rng.below(len.max(1)) as u64,
            rng.below(len.max(1)) as u64,
        ];
        replacements.sort_unstable();
        replacements.dedup();
        for new in replacements.into_iter().filter(|&n| n != value) {
            let mut d = data.get(..from).unwrap_or(&[]).to_vec();
            d.extend_from_slice(new.to_string().as_bytes());
            d.extend_from_slice(data.get(to..).unwrap_or(&[]));
            out.push(Variant { family: "startxref changed", label: format!("sx{new}"), data: d });
        }
    }

    // 3. The cross-reference data scrambled: a window starting where
    // startxref says (the table or the stream), else at the last `xref`.
    let xref_at = startxref_value(data)
        .map(|(_, _, v)| v as usize)
        .filter(|&v| v < len)
        .or_else(|| find_last(data, b"xref"));
    if let Some(at) = xref_at {
        for round in 0..4 {
            let mut d = data.to_vec();
            let window = 200 + 300 * round;
            let end = (at + window).min(len);
            let rate = [3, 3, 2, 8][round];
            for i in at..end {
                if rng.below(rate) == 0 {
                    let b = if round % 2 == 0 { b'0' + rng.below(10) as u8 } else { rng.below(256) as u8 };
                    if let Some(slot) = d.get_mut(i) {
                        *slot = b;
                    }
                }
            }
            out.push(Variant { family: "xref scrambled", label: format!("xr{round}"), data: d });
        }
    }

    // 4. Random byte changes anywhere.
    for (round, count) in [1usize, 1, 3, 10, 40, 150].into_iter().enumerate() {
        let mut d = data.to_vec();
        for _ in 0..count {
            let i = rng.below(len.max(1));
            if let Some(slot) = d.get_mut(i) {
                *slot = if rng.below(2) == 0 { *slot ^ (1 << rng.below(8)) } else { rng.below(256) as u8 };
            }
        }
        out.push(Variant { family: "bytes changed", label: format!("flip{round}"), data: d });
    }
    out
}

#[test]
fn damaged_files_open_or_fail_cleanly_and_quickly() {
    let files = common::public_files();
    if files.is_empty() {
        println!("NOTE: tests/corpus/public is empty; nothing to damage");
        return;
    }
    let dump = std::env::var_os("QINGPDF_WRITE_BROKEN").is_some();
    let dump_dir = common::corpus_root().join("broken");
    if dump {
        std::fs::create_dir_all(&dump_dir).expect("cannot create tests/corpus/broken");
    }

    // family -> outcome description -> count
    let mut table: BTreeMap<&'static str, BTreeMap<String, usize>> = BTreeMap::new();
    let mut problems: Vec<String> = Vec::new();
    let mut slowest = (Duration::ZERO, String::new());
    let mut total = 0usize;

    for (index, path) in files.iter().enumerate() {
        let name = common::corpus_name(path).replace('/', "_");
        let Ok(original) = std::fs::read(path) else { continue };
        let mut rng = common::XorShift::new(0x9E37_79B9_7F4A_7C15 ^ (index as u64 + 1).wrapping_mul(0x1000_0000_01B3));
        for variant in variants(&original, &mut rng) {
            total += 1;
            let case = format!("{name} [{} {}]", variant.family, variant.label);
            if dump {
                let file = dump_dir.join(format!("{name}.{}.pdf", variant.label));
                std::fs::write(file, &variant.data).expect("cannot write a broken file");
            }
            let (outcome, took) = exercise_with_limit(variant.data);
            if took > slowest.0 {
                slowest = (took, case.clone());
            }
            let description = match &outcome {
                None => {
                    problems.push(format!("{case}: no answer after {TIME_LIMIT:?}"));
                    "HANG".to_string()
                }
                Some(o) => {
                    if took > TIME_LIMIT {
                        problems.push(format!("{case}: took {took:?}"));
                    }
                    match o {
                        Outcome::Refused(k) => format!("error ({k})"),
                        Outcome::OpenedAndCopied => "opened, copied, copy reopens".to_string(),
                        Outcome::OpenedCopyRefused(k) => format!("opened, copy refused ({k})"),
                        Outcome::CopyDoesNotReopen(why) => {
                            problems.push(format!("{case}: the copy does not reopen: {why}"));
                            "FAIL copy does not reopen".to_string()
                        }
                        Outcome::Panicked(message) => {
                            problems.push(format!("{case}: PANIC {message}"));
                            "PANIC".to_string()
                        }
                    }
                }
            };
            *table.entry(variant.family).or_default().entry(description).or_default() += 1;
        }
    }

    println!("\n=== damaged files: {} public files, {total} damaged copies ===", files.len());
    for (family, outcomes) in &table {
        let n: usize = outcomes.values().sum();
        println!("{family} ({n})");
        for (description, count) in outcomes {
            println!("  {count:>5}  {description}");
        }
    }
    println!("slowest: {:?} on {}", slowest.0, slowest.1);
    println!("panics: {}, hangs or over 5 s: {}", table.values().filter_map(|m| m.get("PANIC")).sum::<usize>(),
        table.values().filter_map(|m| m.get("HANG")).sum::<usize>());
    assert!(problems.is_empty(), "{} problem(s):\n{}", problems.len(), problems.join("\n"));
}
