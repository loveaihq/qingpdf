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
//! 200 KB bigger than the 833,024 bytes it was before encryption.

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
        inputs.push(Input { name: "source", doc });
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
    // Layer 1.5: at most 200 KB more than the 833,024 bytes before encryption.
    const BEFORE_ENCRYPTION: u64 = 833_024;
    let grown = bytes.saturating_sub(BEFORE_ENCRYPTION);
    let growth_ok = grown <= 200 * 1000;
    println!("  grown by encryption:      {grown} bytes ({:.0} KB)  (target <= 200 KB)  {}", grown as f64 / 1000.0, verdict(growth_ok));

    let encrypted_ok = encrypted()?;

    if info_ok && merge_ok && size_ok && growth_ok && encrypted_ok { Ok(()) } else { Err("a performance target was missed".to_string()) }
}
