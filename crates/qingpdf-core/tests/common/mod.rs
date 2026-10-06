//! Helpers shared by the integration tests: where the test files are, how to
//! find qpdf, and a small seeded random number generator.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root (this file is included from crates two levels down).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

pub fn corpus_root() -> PathBuf {
    workspace_root().join("tests").join("corpus")
}

/// Every `.pdf` below `dir`, sorted. Missing folders give nothing. Read-only.
pub fn walk_pdfs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// `tests/corpus/public` only: files that may be committed.
pub fn public_files() -> Vec<PathBuf> {
    walk_pdfs(&corpus_root().join("public"))
}

/// Public, local and private corpora (the last also from the folder named by
/// `QINGPDF_PRIVATE_CORPUS`), whichever exist.
pub fn corpus_files() -> Vec<PathBuf> {
    let mut dirs = vec![corpus_root().join("public"), corpus_root().join("local"), corpus_root().join("private")];
    if let Some(extra) = std::env::var_os("QINGPDF_PRIVATE_CORPUS") {
        dirs.push(PathBuf::from(extra));
    }
    let mut all = Vec::new();
    for d in dirs {
        all.extend(walk_pdfs(&d));
    }
    all
}

/// A path relative to the corpus folder, with forward slashes, for reports.
pub fn corpus_name(path: &Path) -> String {
    let root = corpus_root();
    path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
}

/// `tests/out/<name>`, created and emptied (it is git-ignored).
pub fn fresh_out_dir(name: &str) -> PathBuf {
    let dir = workspace_root().join("tests").join("out").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cannot create the test output folder");
    dir
}

/// Where qpdf is: the file named by `QINGPDF_QPDF`, else the portable copy in
/// `C:\Dev\tools\qpdf\bin`, else the PATH, else `C:\Program Files\qpdf*\bin`.
pub fn find_qpdf() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "qpdf.exe" } else { "qpdf" };
    if let Some(named) = std::env::var_os("QINGPDF_QPDF") {
        let named = PathBuf::from(named);
        if named.is_file() {
            return Some(named);
        }
    }
    let portable = Path::new("C:/Dev/tools/qpdf/bin").join(exe);
    if portable.is_file() {
        return Some(portable);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for base in [r"C:\Program Files", r"C:\Program Files (x86)"] {
        let Ok(entries) = std::fs::read_dir(base) else { continue };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase().starts_with("qpdf") {
                let candidate = entry.path().join("bin").join(exe);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// What `qpdf --check` said.
pub struct QpdfVerdict {
    /// 0 fine, 2 errors, 3 warnings only.
    pub code: i32,
    pub text: String,
}

pub fn qpdf_check(qpdf: &Path, file: &Path) -> QpdfVerdict {
    let out = Command::new(qpdf).arg("--check").arg(file).output().expect("cannot run qpdf");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    QpdfVerdict { code: out.status.code().unwrap_or(-1), text }
}

/// xorshift64*: tiny, seeded, repeatable. Not for anything but test data.
pub struct XorShift(pub u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        XorShift(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n.max(1) as u64)) as usize
    }
}
