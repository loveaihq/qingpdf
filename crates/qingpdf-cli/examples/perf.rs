//! Acceptance 5: how fast and how small. Builds a 1000-page file and two
//! 500-page files from the test corpus with our own merge, then times the real
//! program on them.
//!
//!   cargo build --release
//!   cargo run --release --example perf -p qingpdf-cli
//!
//! Targets: `info` on 1000 pages <= 0.3 s, `merge` of two 500-page files
//! <= 2 s, release `qingpdf.exe` <= 3 MB. For encrypted files (layer 1.5):
//! opening one costs at most 5 ms more than opening the same file unencrypted,
//! AES decryption runs at 1 GB/s or better, and `qingpdf.exe` is at most
//! 200 KB bigger than the 833,024 bytes it was before encryption. For text
//! extraction (layer 2): all the text of the 468-page e-book
//! `local/zh/ebook/ebook-wikisource-yijikao-468p.pdf` in 1 s or less (without
//! that file, the largest text file of the public corpus, with the time
//! scaled by its page count), and `qingpdf.exe` at most 1 MB bigger than the
//! 994,816 bytes it was before the text tables. Memory (target 200 MB) is
//! measured from outside: `python measure.py -- qingpdf.exe text <file> -o <out>`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use qingpdf_core::ops::{self, Input};
use qingpdf_core::{Builder, Dict, Document, ObjRef, Object, Stream};

const RUNS: usize = 7;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn pdfs_below(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            pdfs_below(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            out.push(path);
        }
    }
}

/// The qingpdf program built next to this example (target/release/qingpdf).
fn program() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) { "qingpdf.exe" } else { "qingpdf" };
    let dir = exe.parent().and_then(Path::parent).ok_or("cannot find the target folder")?;
    let program = dir.join(name);
    if program.is_file() { Ok(program) } else { Err(format!("{} not found: run `cargo build --release` first", program.display())) }
}

/// Run the program `RUNS` times; (fastest, median).
fn time_runs(program: &Path, args: &[&std::ffi::OsStr]) -> Result<(Duration, Duration), String> {
    let mut times = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        let out = Command::new(program).args(args).output().map_err(|e| e.to_string())?;
        let took = started.elapsed();
        if !out.status.success() {
            return Err(format!("qingpdf failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        times.push(took);
    }
    times.sort();
    let fastest = times.first().copied().unwrap_or_default();
    let median = times.get(times.len() / 2).copied().unwrap_or_default();
    Ok((fastest, median))
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

/// The average time of `f` over `n` runs, after a warm-up.
fn average(n: u32, mut f: impl FnMut()) -> Duration {
    for _ in 0..n.min(20) {
        f();
    }
    let started = Instant::now();
    for _ in 0..n {
        f();
    }
    started.elapsed() / n
}

/// Layer 2: the time of `qingpdf text` on the longest text file we have.
fn text_case(program: &Path) -> Result<bool, String> {
    let corpus = root().join("tests").join("corpus");
    let book = corpus.join("local").join("zh").join("ebook").join("ebook-wikisource-yijikao-468p.pdf");
    let (file, pages, note) = if book.is_file() {
        let pages = Document::open(&book).and_then(|d| d.page_count()).map_err(|e| e.to_string())?;
        (book, pages, "")
    } else {
        // The public file with the most pages that has a text layer (a scan gives no text).
        let mut all = Vec::new();
        pdfs_below(&corpus.join("public"), &mut all);
        let mut best: Option<(PathBuf, usize)> = None;
        for path in all {
            let Ok(doc) = Document::open(&path) else { continue };
            if doc.is_encrypted() {
                continue;
            }
            let Ok(count) = doc.page_count() else { continue };
            let has_text = doc.pages().ok().and_then(|p| p.first().and_then(|first| qingpdf_core::text::TextExtractor::new(&doc).page_text(first).ok())).is_some_and(|t| !t.is_empty());
            if has_text && best.as_ref().is_none_or(|(_, n)| count > *n) {
                best = Some((path, count));
            }
        }
        let (path, count) = best.ok_or("no file with a text layer")?;
        (path, count, " (the 468-page e-book is not here; the largest public text file instead)")
    };
    let out = root().join("tests").join("out").join("perf").join("text.txt");
    std::fs::create_dir_all(out.parent().ok_or("no folder")?).map_err(|e| e.to_string())?;
    let (fastest, median) = time_runs(program, &["text".as_ref(), file.as_os_str(), "-o".as_ref(), out.as_os_str(), "--force".as_ref()])?;
    let limit = (pages as f64 / 468.0).clamp(0.05, 1.0);
    let ok = median.as_secs_f64() <= limit;
    println!("\ntext, {} ({pages} pages){note}:", file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    println!("  all pages to a file:      fastest {fastest:.3?}, median of {RUNS} {median:.3?}  (target <= {limit:.2} s)  {}", verdict(ok));
    Ok(ok)
}

/// One page drawn at 150 dpi, inside this process (the drawing alone, and the first drawing of all, which reads
/// the system fonts) and by the program (start, drawing, PNG file). Returns whether the targets were met.
fn render_case(program: &Path) -> Result<bool, String> {
    let corpus = root().join("tests").join("corpus");
    let out_dir = root().join("tests").join("out").join("perf");
    // (label, file, page, target in ms for the drawing)
    let cases = [
        ("text page, Type 1 fonts embedded (tracemonkey, English)", corpus.join("local").join("xref-classic").join("tracemonkey.pdf"), 1, 50.0),
        ("text page, TrueType fonts embedded (a government report, Chinese)", corpus.join("public").join("zh").join("gongwen").join("gongwen-2024-nicheng-work-report.pdf"), 2, 50.0),
        ("text page, fonts embedded as CFF (a LaTeX paper, Chinese and English)", corpus.join("public").join("zh").join("lunwen").join("lunwen-arxiv-2601.14329-latex.pdf"), 1, 50.0),
        ("text page, Chinese not embedded (SimSun from the system, a paper)", corpus.join("local").join("zh").join("lunwen").join("lunwen-arxiv-2410.20383-cnki-ttkn.pdf"), 1, 50.0),
        ("text page, Arial and Times not embedded (a Word paper)", corpus.join("public").join("zh").join("lunwen").join("lunwen-arxiv-2403.14268-word-tc.pdf"), 2, 50.0),
        ("text page, tricky TrueType font run with its instructions (3b2: DFKaiShu title and authors, a Word paper)", corpus.join("public").join("zh").join("lunwen").join("lunwen-arxiv-2403.14268-word-tc.pdf"), 1, 50.0),
        ("scanned page, JPEG, 1242 x 1754 px", corpus.join("local").join("scanned").join("issue7229.pdf"), 1, 150.0),
        ("scanned page, CCITT G4, A4 at 300 dpi", out_dir.join("scan-ccitt-a4-300dpi.pdf"), 1, 150.0),
    ];
    let mut all_ok = true;
    println!("
render, one page at 150 dpi:");
    for (label, file, page_number, target_ms) in cases {
        if !file.is_file() {
            println!("  {label}: {} is not here, skipped (tests/tools/make_scan_fixture.py makes the CCITT one)", file.display());
            continue;
        }
        let doc = Document::open(&file).map_err(|e| e.to_string())?;
        let pages = doc.pages().map_err(|e| e.to_string())?;
        let page = pages.get(page_number - 1).ok_or("no such page")?;
        // The first drawing of the process: reads the system fonts this page needs (the files are in the system's cache).
        let started = Instant::now();
        let mut first_renderer = qingpdf_core::render::Renderer::new(&doc);
        let first_result = first_renderer.render_page(page, 150.0).map_err(|e| e.to_string())?;
        let first = started.elapsed().as_secs_f64() * 1000.0;
        let drawn = average(20, || {
            let mut r = qingpdf_core::render::Renderer::new(&doc);
            let _ = r.render_page(page, 150.0);
        });
        let png = out_dir.join("render.png");
        let page_arg = page_number.to_string();
        let (fastest, median) = time_runs(
            program,
            &["render".as_ref(), file.as_os_str(), "--pages".as_ref(), page_arg.as_ref(), "--dpi".as_ref(), "150".as_ref(), "-o".as_ref(), png.as_os_str(), "--force".as_ref()],
        )?;
        let ms = drawn.as_secs_f64() * 1000.0;
        let ok = ms <= target_ms && first <= target_ms * 1.5;
        all_ok &= ok;
        println!("  {label}:");
        println!("    the first drawing in the process (system fonts read):  {first:.1} ms  (boxes {}, glyphs not in their fonts {})", first_result.boxed_characters, first_result.absent_glyphs);
        println!("    drawing alone (average of 20): {ms:.1} ms  (target <= {target_ms:.0} ms)  {}", verdict(ok));
        println!("    the program, start to PNG on disk: fastest {fastest:.3?}, median of {RUNS} {median:.3?}");
    }
    all_ok &= book_case(&corpus)?;
    Ok(all_ok)
}

/// The first 100 pages of the 468-page e-book at 150 dpi, one renderer for all of them: the average time of a page.
fn book_case(corpus: &Path) -> Result<bool, String> {
    let book = corpus.join("local").join("zh").join("ebook").join("ebook-wikisource-yijikao-468p.pdf");
    if !book.is_file() {
        println!("  the 468-page e-book is not here, skipped");
        return Ok(true);
    }
    let doc = Document::open(&book).map_err(|e| e.to_string())?;
    let pages = doc.pages().map_err(|e| e.to_string())?;
    let mut renderer = qingpdf_core::render::Renderer::new(&doc);
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    let count = pages.len().min(100);
    for page in pages.iter().take(count) {
        let t = Instant::now();
        renderer.render_page(page, 150.0).map_err(|e| e.to_string())?;
        slowest = slowest.max(t.elapsed());
    }
    let total = started.elapsed();
    let per_page = total.as_secs_f64() * 1000.0 / count as f64;
    println!("  e-book (468 pages), the first {count} pages in one renderer: {:.2} s, {per_page:.1} ms a page on average, slowest {:.1} ms  (target <= 50 ms a page)  {}", total.as_secs_f64(), slowest.as_secs_f64() * 1000.0, verdict(per_page <= 50.0));
    Ok(per_page <= 50.0)
}

/// Encrypted files: the cost of opening them, and how fast AES decrypts.
/// Returns whether the targets were met.
fn encrypted() -> Result<bool, String> {
    let public = root().join("tests").join("corpus").join("public");
    let dir = public.join("encrypted").join("qpdf-generated");
    // The same document, plain and encrypted by qpdf: opening is reading the
    // cross-reference data and the encryption dictionary, trying the empty
    // password first, and checking the catalog.
    let cases = [
        ("RC4 40-bit (R2)", "bookmarks.r2-40-empty-print-none.pdf", "outline-form-attach/bookmarks.pdf", ""),
        ("RC4 128-bit (R3)", "rects.r3-128-rc4-empty-extract-n.pdf", "page-tree/rectangles_multi_pages.pdf", ""),
        ("AES-128 (R4), object streams", "objstm.r4-aes128-empty-print-none.pdf", "xref-stream-objstm/bug_757705.pdf", ""),
        ("AES-256 (R5), object streams", "objstm.r5-aes256-user.pdf", "xref-stream-objstm/bug_757705.pdf", "user"),
        ("AES-256 (R6), object streams", "objstm.r6-aes256-empty-extract-n.pdf", "xref-stream-objstm/bug_757705.pdf", ""),
        ("AES-256 (R6), user password", "pdf20utf8.r6-aes256-user-modify-none.pdf", "pdf20/pdf20-utf8-test.pdf", "user"),
        ("AES-256 (R6), Chinese password", "vertical.r6-aes256-user-chinese.pdf", "cjk/vertical.pdf", "密码"),
    ];
    let mut all_ok = true;
    println!("\nopening encrypted files (average of 500 runs, in this process):");
    for (what, encrypted, original, password) in cases {
        let secret = std::fs::read(dir.join(encrypted)).map_err(|e| e.to_string())?;
        let clear = std::fs::read(public.join(original)).map_err(|e| e.to_string())?;
        let open = |bytes: &[u8], password: &str| {
            let doc = Document::from_bytes_with_password(bytes.to_vec(), password).map_err(|e| e.to_string());
            let _ = std::hint::black_box(doc.map(|d| d.page_count()));
        };
        let t_clear = average(500, || open(&clear, ""));
        let t_secret = average(500, || open(&secret, password));
        let extra = t_secret.saturating_sub(t_clear);
        let ok = extra.as_secs_f64() <= 0.005;
        all_ok &= ok;
        println!("  {what:<34} plain {t_clear:>9.1?}  encrypted {t_secret:>9.1?}  extra {extra:>9.1?}  (target <= 5 ms)  {}", verdict(ok));
    }

    // Revision 6 with the password that has to be found: the user's, the owner's
    // (which is the slow one: its hash covers more data), Chinese ones, one that
    // needs the second way of writing it (full-width letters), a long one, and a
    // wrong one (an error, and what it costs to find out).
    let owner_cases: [(&str, &str, &str, &str, Option<f64>); 8] = [
        ("user password", "pdf20utf8.r6-aes256-user-modify-none.pdf", "pdf20/pdf20-utf8-test.pdf", "user", Some(3.0)),
        ("owner password", "pdf20utf8.r6-aes256-user-modify-none.pdf", "pdf20/pdf20-utf8-test.pdf", "owner", Some(5.0)),
        ("Chinese user password", "vertical.r6-aes256-user-chinese.pdf", "cjk/vertical.pdf", "密码", Some(3.0)),
        ("Chinese owner password", "vertical.r6-aes256-user-chinese.pdf", "cjk/vertical.pdf", "主人", Some(5.0)),
        ("full-width form of the user password", "utf8.r6-aes256-user-fullwidth.pdf", "outline-form-attach/utf-8.pdf", "ＡＢＣ１２３", None),
        ("127 bytes of password", "utf8.r6-aes256-user-127bytes.pdf", "outline-form-attach/utf-8.pdf", &format!("{}0123456", "0123456789".repeat(12)), None),
        ("wrong password", "pdf20utf8.r6-aes256-user-modify-none.pdf", "pdf20/pdf20-utf8-test.pdf", "not the password", None),
        ("no password given (locked)", "pdf20utf8.r6-aes256-user-modify-none.pdf", "pdf20/pdf20-utf8-test.pdf", "", None),
    ];
    println!("\nopening a revision 6 file by the password (target for the first four: user <= 3 ms, owner <= 5 ms):");
    for (what, encrypted, original, password, target) in owner_cases {
        let secret = std::fs::read(dir.join(encrypted)).map_err(|e| e.to_string())?;
        let clear = std::fs::read(public.join(original)).map_err(|e| e.to_string())?;
        let open = |bytes: &[u8], password: &str| {
            let doc = Document::from_bytes_with_password(bytes.to_vec(), password).map_err(|e| e.to_string());
            let _ = std::hint::black_box(doc.map(|d| d.page_count()));
        };
        let t_clear = average(300, || open(&clear, ""));
        let t_secret = average(300, || open(&secret, password));
        let extra = t_secret.saturating_sub(t_clear);
        let (limit, text) = match target {
            Some(ms) => (ms, format!("(target <= {ms} ms)  {}", verdict(extra.as_secs_f64() * 1000.0 <= ms))),
            None => (f64::MAX, "(no target)".to_string()),
        };
        all_ok &= extra.as_secs_f64() * 1000.0 <= limit;
        println!("  {what:<40} plain {t_clear:>9.1?}  encrypted {t_secret:>9.1?}  extra {extra:>9.1?}  {text}");
    }

    // AES decryption: a 128 MB stream written encrypted (with the keys of three
    // of those files), then read: the time `get` takes, less what it takes for
    // the same stream unencrypted (reading it and copying it).
    println!("\ndecryption of one 128 MB stream (reading it, less the same stream unencrypted):");
    let size = 128usize << 20;
    let data: Vec<u8> = (0..size as u64).map(|i| (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8).collect();
    for (what, file) in [
        ("AES-256", "bookmarks.r6-aes256-empty-print-none.pdf"),
        ("AES-128", "objstm.r4-aes128-empty-print-none.pdf"),
        ("RC4 128-bit", "rects.r3-128-rc4-empty-extract-n.pdf"),
    ] {
        let source = Document::open(dir.join(file)).map_err(|e| e.to_string())?;
        let build = |encrypt: bool| -> Result<(Vec<u8>, ObjRef), String> {
            let pages = HashSet::new();
            let mut b = Builder::new((1, 7));
            let src = b.add_source(&source, &pages);
            if encrypt {
                b.keep_encryption_of(src);
            }
            let big = b.add(&Object::Stream(Stream { dict: Dict::new(), data: data.clone() })).map_err(|e| e.to_string())?;
            let root = b.reserve().map_err(|e| e.to_string())?;
            let mut catalog = Dict::new();
            catalog.set("Type", Object::from("Catalog"));
            b.put(root, &Object::Dict(catalog)).map_err(|e| e.to_string())?;
            let (bytes, _) = b.finish(root, None).map_err(|e| e.to_string())?;
            Ok((bytes, big))
        };
        let (encrypted_file, big) = build(true)?;
        let (plain_file, _) = build(false)?;
        let read = |bytes: &[u8]| -> Result<Duration, String> {
            let doc = Document::from_bytes(bytes.to_vec()).map_err(|e| e.to_string())?;
            let mut best = Duration::MAX;
            for _ in 0..5 {
                let started = Instant::now();
                let object = doc.get(big).map_err(|e| e.to_string())?;
                best = best.min(started.elapsed());
                match object {
                    Object::Stream(s) if s.data == data => {}
                    _ => return Err("the big stream did not come back as it went in".to_string()),
                }
            }
            Ok(best)
        };
        let (t_plain, t_encrypted) = (read(&plain_file)?, read(&encrypted_file)?);
        let decrypting = t_encrypted.saturating_sub(t_plain);
        let gb_per_s = size as f64 / decrypting.as_secs_f64().max(1e-9) / 1e9;
        let whole = size as f64 / t_encrypted.as_secs_f64() / 1e9;
        let aes = what != "RC4 128-bit";
        let ok = !aes || gb_per_s >= 1.0;
        all_ok &= ok;
        let target = if aes { format!("(target >= 1 GB/s)  {}", verdict(ok)) } else { "(RC4: no target)".to_string() };
        println!(
            "  {what:<12} read plain {t_plain:>9.1?}  read encrypted {t_encrypted:>9.1?}  decryption alone {gb_per_s:>5.2} GB/s (whole read {whole:.2} GB/s)  {target}"
        );
    }
    Ok(all_ok)
}

fn main() -> Result<(), String> {
    let program = program()?;

    // Source documents: the multi-page corpus files of moderate size.
    let mut files = Vec::new();
    pdfs_below(&root().join("tests").join("corpus"), &mut files);
    files.sort();
    let mut sources: Vec<(PathBuf, Document, usize)> = Vec::new();
    for path in files {
        let small = std::fs::metadata(&path).map(|m| m.len() <= 4_000_000).unwrap_or(false);
        if !small {
            continue;
        }
        let Ok(doc) = Document::open(&path) else { continue };
        if doc.is_encrypted() {
            continue;
        }
        let Ok(pages) = doc.pages() else { continue };
        if !pages.is_empty() {
            let n = pages.len();
            sources.push((path, doc, n));
        }
    }
    sources.sort_by_key(|s| std::cmp::Reverse(s.2));
    sources.truncate(8);
    if sources.is_empty() {
        return Err("no corpus files to build the test files from".to_string());
    }
    println!("building test files from:");
    for (path, _, n) in &sources {
        println!("  {} ({n} pages)", path.display());
    }

    // 1200 pages by merging the sources round and round, then cut to size.
    let mut inputs = Vec::new();
    let mut total = 0usize;
    let mut i = 0usize;
    while total < 1200 {
        let (path, doc, n) = sources.get(i % sources.len()).ok_or("no sources")?;
        inputs.push(Input { name: "source", doc, password: "" });
        total += n;
        i += 1;
        let _ = path;
    }
    let merged = ops::merge(&inputs).map_err(|e| e.to_string())?;
    let big = Document::from_bytes(merged.data).map_err(|e| e.to_string())?;
    let pick = |from: usize, to: usize| -> Result<Vec<u8>, String> {
        let pages: Vec<usize> = (from..to).collect();
        Ok(ops::extract_pages(&big, &pages).map_err(|e| e.to_string())?.data)
    };
    let dir = root().join("tests").join("out").join("perf");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let thousand = dir.join("pages-1000.pdf");
    let half_a = dir.join("pages-500-a.pdf");
    let half_b = dir.join("pages-500-b.pdf");
    let out = dir.join("merged-1000.pdf");
    for (path, from, to) in [(&thousand, 0, 1000), (&half_a, 0, 500), (&half_b, 500, 1000)] {
        let bytes = pick(from, to)?;
        println!("wrote {} ({} pages, {:.1} MB)", path.display(), to - from, bytes.len() as f64 / 1e6);
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    }

    // info on 1000 pages.
    let (fastest, median) = time_runs(&program, &["info".as_ref(), thousand.as_os_str()])?;
    let info_ok = median.as_secs_f64() <= 0.3;
    println!("\ninfo, 1000 pages:           fastest {fastest:.3?}, median of {RUNS} {median:.3?}  (target <= 0.3 s)  {}", verdict(info_ok));

    // merge of two 500-page files.
    let (fastest, median) = time_runs(
        &program,
        &["merge".as_ref(), half_a.as_os_str(), half_b.as_os_str(), "-o".as_ref(), out.as_os_str(), "--force".as_ref()],
    )?;
    let merge_ok = median.as_secs_f64() <= 2.0;
    println!("merge, 500 + 500 pages:     fastest {fastest:.3?}, median of {RUNS} {median:.3?}  (target <= 2 s)    {}", verdict(merge_ok));
    let merged_pages = Document::open(&out).map_err(|e| e.to_string())?.page_count().map_err(|e| e.to_string())?;
    println!("  merged file: {merged_pages} pages, {:.1} MB", std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) as f64 / 1e6);

    // size of the program.
    let bytes = std::fs::metadata(&program).map_err(|e| e.to_string())?.len();
    let size_ok = bytes <= 3 * 1024 * 1024;
    println!("qingpdf.exe size:           {bytes} bytes = {:.2} MB  (target <= 3 MB)  {}", bytes as f64 / 1_048_576.0, verdict(size_ok));
    // Layer 1.5: at most 200 KB more than the 833,024 bytes before encryption. The program now also holds
    // the text tables, so the growth is taken from the size recorded when layer 1.5 was finished.
    const BEFORE_ENCRYPTION: u64 = 833_024;
    const BEFORE_TEXT: u64 = 994_816;
    let grown = BEFORE_TEXT.saturating_sub(BEFORE_ENCRYPTION);
    let growth_ok = grown <= 200 * 1000;
    println!("  grown by encryption:      {grown} bytes ({:.0} KB)  (target <= 200 KB; the size recorded at layer 1.5)  {}", grown as f64 / 1000.0, verdict(growth_ok));
    // Layer 2: at most 1 MB more than the 994,816 bytes before the text tables (the size recorded when layer 2
    // was finished, 1,322,496 bytes with the later patch).
    const BEFORE_RENDER: u64 = 1_322_496;
    let text_grown = BEFORE_RENDER.saturating_sub(BEFORE_TEXT);
    let text_growth_ok = text_grown <= 1_000_000;
    println!("  grown by text extraction: {text_grown} bytes ({:.0} KB)  (target <= 1 MB; the size recorded at layer 2)  {}", text_grown as f64 / 1000.0, verdict(text_growth_ok));
    // Layer 3, step 3a: at most 1 MB more than the 1,322,496 bytes before rendering.
    let render_grown = bytes.saturating_sub(BEFORE_RENDER);
    let render_growth_ok = render_grown <= 1_000_000;
    println!("  grown by rendering (3a):  {render_grown} bytes ({:.0} KB)  (target <= 1 MB)  {}", render_grown as f64 / 1000.0, verdict(render_growth_ok));

    // Layer 3, step 3b: at most 0.6 MB more than the 2,057,216 bytes before the fonts.
    const BEFORE_FONTS: u64 = 2_057_216;
    let fonts_grown = bytes.saturating_sub(BEFORE_FONTS);
    let fonts_growth_ok = fonts_grown <= 600_000;
    println!("  grown by fonts (3b):      {fonts_grown} bytes ({:.0} KB)  (target <= 0.6 MB)  {}", fonts_grown as f64 / 1000.0, verdict(fonts_growth_ok));

    // Layer 3, step 3b2: at most 0.15 MB more than the 2,205,696 bytes before the instruction interpreter.
    const BEFORE_TRICKY: u64 = 2_205_696;
    let tricky_grown = bytes.saturating_sub(BEFORE_TRICKY);
    let tricky_growth_ok = tricky_grown <= 150_000;
    println!("  grown by tricky fonts (3b2): {tricky_grown} bytes ({:.0} KB)  (target <= 0.15 MB)  {}", tricky_grown as f64 / 1000.0, verdict(tricky_growth_ok));

    let encrypted_ok = encrypted()?;
    let text_ok = text_case(&program)?;
    let render_ok = render_case(&program)?;

    if info_ok && merge_ok && size_ok && growth_ok && text_growth_ok && render_growth_ok && fonts_growth_ok && tricky_growth_ok && encrypted_ok && text_ok && render_ok { Ok(()) } else { Err("a performance target was missed".to_string()) }
}
