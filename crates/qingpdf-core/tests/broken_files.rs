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

// --- hostile files from the review: every one of them within seconds, and either read or a Limit ---------
//
// Hard rules 4 and 6: a crafted file must not hang the program or eat gigabytes.
// The files are made here, so the test needs nothing from outside. The time
// limits are for the optimised build (`cargo test --release`); an unoptimised
// build gets ten times as long.

mod hostile {
    use std::time::{Duration, Instant};

    use miniz_oxide::deflate::core::{CompressorOxide, TDEFLFlush, TDEFLStatus, compress, create_comp_flags_from_zip_params};
    use qingpdf_core::{Document, Error, ObjRef, Object, ops};

    fn limit() -> Duration {
        if cfg!(debug_assertions) { Duration::from_secs(60) } else { Duration::from_secs(5) }
    }

    /// zlib data for `head` followed by `zeros` zero bytes, made without ever
    /// holding the uncompressed bytes.
    fn zlib_head_and_zeros(head: &[u8], zeros: usize) -> Vec<u8> {
        let flags = create_comp_flags_from_zip_params(1, 1, 0);
        let mut comp = Box::new(CompressorOxide::new(flags));
        let mut out: Vec<u8> = Vec::new();
        let mut scratch = vec![0u8; 1 << 16];
        let mut feed = |comp: &mut CompressorOxide, mut data: &[u8], flush: TDEFLFlush, out: &mut Vec<u8>| loop {
            let (status, used, produced) = compress(comp, data, &mut scratch, flush);
            out.extend_from_slice(&scratch[..produced]);
            data = &data[used..];
            if matches!(status, TDEFLStatus::Done) || (data.is_empty() && produced < scratch.len() && flush != TDEFLFlush::Finish) {
                break;
            }
        };
        feed(&mut comp, head, TDEFLFlush::None, &mut out);
        let chunk = vec![0u8; 1 << 20];
        let mut left = zeros;
        while left > 0 {
            let n = left.min(chunk.len());
            feed(&mut comp, &chunk[..n], TDEFLFlush::None, &mut out);
            left -= n;
        }
        feed(&mut comp, &[], TDEFLFlush::Finish, &mut out);
        out
    }

    /// Builds a file object by object.
    struct Pdf {
        buf: Vec<u8>,
    }

    impl Pdf {
        fn new() -> Pdf {
            Pdf { buf: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec() }
        }

        fn obj(&mut self, num: u32, body: &str) -> usize {
            let at = self.buf.len();
            self.buf.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
            at
        }

        fn stream(&mut self, num: u32, dict: &str, data: &[u8]) -> usize {
            let at = self.buf.len();
            self.buf.extend_from_slice(format!("{num} 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes());
            self.buf.extend_from_slice(data);
            self.buf.extend_from_slice(b"\nendstream\nendobj\n");
            at
        }

        fn finish(mut self, startxref: usize) -> Vec<u8> {
            self.buf.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
            self.buf
        }
    }

    fn run_within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> (T, Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        let value = rx.recv_timeout(limit() * 3).expect("no answer: the program hangs");
        (value, started.elapsed())
    }

    fn assert_quick(took: Duration, what: &str) {
        assert!(took < limit(), "{what} took {took:?} (limit {:?})", limit());
    }

    /// For the one test that has to decode a whole gigabyte before the
    /// document's budget runs out: that takes seconds even when nothing is
    /// wrong, so it gets four times the time (still a fraction of what the
    /// unbounded decoding took).
    fn assert_within_four_times(took: Duration, what: &str) {
        assert!(took < limit() * 4, "{what} took {took:?} (limit {:?})", limit() * 4);
    }

    /// Review finding h1: 300 pages that all inherit a /Resources of 10 MB.
    #[test]
    fn a_huge_inherited_resources_dictionary_is_shared_not_copied_into_every_page() {
        let long_name = "A".repeat(10_000);
        let names: String = (0..1000).map(|_| format!("/{long_name} ")).collect();
        let pages = 300u32;
        let mut p = Pdf::new();
        let mut offsets = vec![0usize; 3 + pages as usize];
        offsets[1] = p.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..pages).map(|i| format!("{} 0 R ", 3 + i)).collect();
        offsets[2] = p.obj(
            2,
            &format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 100 100] /Resources << /X [{names}] >> >>"),
        );
        for i in 0..pages {
            offsets[3 + i as usize] = p.obj(3 + i, "<< /Type /Page /Parent 2 0 R >>");
        }
        let xref = p.buf.len();
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len());
        for o in &offsets[1..] {
            table.push_str(&format!("{o:010} 00000 n \n"));
        }
        table.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\n", offsets.len()));
        p.buf.extend_from_slice(table.as_bytes());
        let bytes = p.finish(xref);
        let input_len = bytes.len();

        let ((rotated, pages_found, count_pages), took) = run_within(move || {
            let doc = Document::from_bytes(bytes).expect("opens");
            let found = doc.pages().expect("pages").len();
            let out = ops::rotate_pages(&doc, &(0..300).collect::<Vec<_>>(), 90).expect("rotate");
            (out.data, found, out.pages)
        });
        assert_quick(took, "reading and rotating 300 pages that share 10 MB of resources");
        assert_eq!((pages_found, count_pages), (300, 300));
        // The 10 MB is in the output once, not 300 times.
        assert!(rotated.len() < input_len * 2, "output {} bytes from {input_len}", rotated.len());
        let doc = Document::from_bytes(rotated).expect("the output opens");
        assert!(!doc.was_repaired());
        assert_eq!(doc.page_count().unwrap(), 300);
    }

    /// Review finding h2 (smaller): object streams that decode to more than the
    /// limit for one object stream are a Limit, quickly, not gigabytes.
    #[test]
    fn object_streams_that_decode_to_more_than_the_limit_are_refused() {
        // Object stream 100 holds the page tree root and decodes to 70 MiB.
        let header = "2 0 ";
        let body = "<< /Type /Pages /Kids [] /Count 0 >>";
        let packed = zlib_head_and_zeros(format!("{header}{body}").as_bytes(), 70 << 20);
        let mut p = Pdf::new();
        let o1 = p.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o100 = p.stream(100, &format!("/Type /ObjStm /N 1 /First {} /Filter /FlateDecode", header.len()), &packed);
        let x = p.buf.len();
        let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
        let mut rows: Vec<[u8; 4]> = vec![row(0, 0, 255), row(1, o1, 0), row(2, 100, 0)];
        rows.resize(100, row(0, 0, 0));
        rows.push(row(1, o100, 0));
        rows.resize(101, row(0, 0, 0));
        rows.push(row(1, x, 0));
        let table: Vec<u8> = rows.iter().flatten().copied().collect();
        p.stream(101, &format!("/Type /XRef /Size {} /W [1 2 1] /Root 1 0 R", rows.len()), &table);
        let bytes = p.finish(x);
        let (outcome, took) = run_within(move || Document::from_bytes(bytes).map(|_| ()));
        assert_quick(took, "opening a file whose page tree is in a 70 MiB object stream");
        assert!(matches!(outcome, Err(Error::Limit(_))), "{outcome:?}");
    }

    /// Review finding h2: a file whose object streams are each below the limit but
    /// that makes the reader decode them again and again runs into the budget
    /// for a whole document.
    #[test]
    fn decoding_the_same_big_object_streams_again_and_again_runs_into_the_document_budget() {
        // 40 object streams, 40 MiB each when decoded (1.6 GiB in all, more than the
        // budget), all the same bytes. Object 10+k is said to be in stream 100+k.
        let n = 40u32;
        let header = "5 0 ";
        let packed = zlib_head_and_zeros(format!("{header}(x)").as_bytes(), 40 << 20);
        let mut p = Pdf::new();
        let o1 = p.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o2 = p.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let mut at = Vec::new();
        for k in 0..n {
            at.push(p.stream(100 + k, &format!("/Type /ObjStm /N 1 /First {} /Filter /FlateDecode", header.len()), &packed));
        }
        let x = p.buf.len();
        let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
        let size = 100 + n + 1;
        let mut rows: Vec<[u8; 4]> = vec![row(0, 0, 0); size as usize];
        rows[0] = row(0, 0, 255);
        rows[1] = row(1, o1, 0);
        rows[2] = row(1, o2, 0);
        for k in 0..n {
            rows[(10 + k) as usize] = row(2, 100 + k as usize, 0);
            rows[(100 + k) as usize] = row(1, at[k as usize], 0);
        }
        rows[(100 + n) as usize] = row(1, x, 0);
        let table: Vec<u8> = rows.iter().flatten().copied().collect();
        p.stream(100 + n, &format!("/Type /XRef /Size {size} /W [1 2 1] /Root 1 0 R"), &table);
        let bytes = p.finish(x);
        let (limited, took) = run_within(move || {
            let doc = Document::from_bytes(bytes).expect("opens");
            let mut limited = 0usize;
            for round in 0..2 {
                for k in 0..n {
                    match doc.get(ObjRef::new(10 + k, 0)) {
                        Err(Error::Limit(_)) => limited += 1,
                        Err(e) => panic!("round {round}, stream {k}: {e}"),
                        Ok(Object::Null | Object::String(_)) => {}
                        Ok(other) => panic!("{other:?}"),
                    }
                }
            }
            limited
        });
        assert_within_four_times(took, "decoding 40 object streams of 40 MiB twice over");
        assert!(limited > 0, "the budget for decoding never ran out");
    }

    /// An xref stream that declares (nearly) as many entries as the limit allows.
    fn xref_stream_file(sections: usize) -> Vec<u8> {
        let n = (1usize << 23) - 8;
        let mut p = Pdf::new();
        let o1 = p.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let o2 = p.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        let o3 = p.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
        let row = |t: u8, a: usize| {
            let mut r = vec![t];
            r.extend_from_slice(&u32::try_from(a).unwrap().to_be_bytes());
            r.extend_from_slice(&[0, 0]);
            r
        };
        let head: Vec<u8> = [row(0, 0), row(1, o1), row(1, o2), row(1, o3)].concat();
        // Every row after these is a free entry: 7 zero bytes each.
        let packed = zlib_head_and_zeros(&head, 7 * (n - 4));
        let mut prev: Option<usize> = None;
        for k in 0..sections {
            let at = p.buf.len();
            let prev_entry = prev.map_or(String::new(), |q| format!("/Prev {q}"));
            p.stream(
                4 + u32::try_from(k).unwrap(),
                &format!("/Type /XRef /Size {n} /W [1 4 2] /Root 1 0 R {prev_entry} /Filter /FlateDecode"),
                &packed,
            );
            prev = Some(at);
        }
        p.finish(prev.unwrap())
    }

    /// Review finding h4: one xref stream with 8 million entries.
    #[test]
    fn an_xref_stream_with_eight_million_entries_is_read_quickly() {
        let bytes = xref_stream_file(1);
        assert!(bytes.len() < 1_000_000, "the file is small ({} bytes): that is the point", bytes.len());
        let (outcome, took) = run_within(move || {
            let doc = Document::from_bytes(bytes)?;
            doc.page_count()
        });
        assert_quick(took, "reading an xref stream with 8 million entries");
        assert!(matches!(outcome, Ok(1)), "{outcome:?}");
    }

    /// Review finding h4b: six of them chained by /Prev; the cap is for the
    /// whole document, not per section.
    #[test]
    fn six_chained_xref_streams_of_eight_million_entries_stay_within_the_cap_for_the_whole_file() {
        let bytes = xref_stream_file(6);
        assert!(bytes.len() < 4_000_000, "{} bytes: still tiny next to what it declares", bytes.len());
        let (outcome, took) = run_within(move || {
            let doc = Document::from_bytes(bytes)?;
            let repaired = doc.was_repaired();
            doc.page_count().map(|n| (n, repaired))
        });
        assert_quick(took, "reading six chained xref streams of 8 million entries");
        // Either the limit is reported, or the file is read by scanning it:
        // the chain beyond the cap is not followed either way.
        match outcome {
            Err(Error::Limit(_)) => {}
            Ok((1, true)) => {}
            other => panic!("{other:?}"),
        }
    }
}
