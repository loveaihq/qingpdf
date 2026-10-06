//! Acceptance 5: how fast and how small. Builds a 1000-page file and two
//! 500-page files from the test corpus with our own merge, then times the real
//! program on them.
//!
//!   cargo build --release
//!   cargo run --release --example perf -p qingpdf-cli
//!
//! Targets: `info` on 1000 pages <= 0.3 s, `merge` of two 500-page files
//! <= 2 s, release `qingpdf.exe` <= 3 MB.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use qingpdf_core::ops::{self, Input};
use qingpdf_core::Document;

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

    if info_ok && merge_ok && size_ok { Ok(()) } else { Err("a performance target was missed".to_string()) }
}
