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

/// `tests/corpus/public` only: files that may be committed. The public corpus
/// is part of the repository, so a missing or empty one is a failure, not a
/// reason to skip the test that needs it.
pub fn public_files() -> Vec<PathBuf> {
    let dir = corpus_root().join("public");
    let files = walk_pdfs(&dir);
    assert!(
        !files.is_empty(),
        "the public test corpus is missing or empty: {} (it is part of the repository)",
        dir.display()
    );
    files
}

/// One file of the public corpus, `rel` being its path below `tests/corpus/public`.
/// It must be there: a missing one fails the test.
pub fn public_file(rel: &str) -> PathBuf {
    let path = corpus_root().join("public").join(rel);
    assert!(path.is_file(), "public corpus file {} is missing", path.display());
    path
}

/// Public, local and private corpora (the last also from the folder named by
/// `QINGPDF_PRIVATE_CORPUS`). The public one must be there; the others are
/// used when they exist.
pub fn corpus_files() -> Vec<PathBuf> {
    let mut all = public_files();
    let mut dirs = vec![corpus_root().join("local"), corpus_root().join("private")];
    if let Some(extra) = std::env::var_os("QINGPDF_PRIVATE_CORPUS") {
        dirs.push(PathBuf::from(extra));
    }
    for d in dirs {
        all.extend(walk_pdfs(&d));
    }
    all
}

/// Does `haystack` contain `needle`?
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
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
#[derive(Clone)]
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

/// [`qpdf_check`] of an input, remembered: the same input is judged again for
/// every output made from it, and qpdf is slow on big files.
fn qpdf_check_input(qpdf: &Path, file: &Path) -> QpdfVerdict {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, QpdfVerdict>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(known) = cache.lock().expect("cache lock").get(file) {
        return known.clone();
    }
    let verdict = qpdf_check(qpdf, file);
    cache.lock().expect("cache lock").insert(file.to_path_buf(), verdict.clone());
    verdict
}

/// One complaint of qpdf (a `WARNING:` line), reduced to what it says: the file
/// name (wherever it appears, as `file` is printed) and the `(object 5 0,
/// offset 120)` part are taken out and every number becomes `N`, so that the
/// same complaint about an input and about the file made from it compares
/// equal. Lower case.
fn normalise_complaint(line: &str, file: &Path) -> String {
    let named = file.display().to_string();
    let line = line.replace(&named, "FILE");
    let line = line.as_str();
    // `WARNING: FILE (<where>): <message>` or `WARNING: FILE: <message>`.
    let rest = line.split_once(": ").map_or(line, |(_, rest)| rest);
    let message = match rest.find("): ") {
        Some(i) if rest.get(..i).is_some_and(|head| head.contains(" (")) => rest.get(i + 3..).unwrap_or(rest),
        _ => rest.split_once(": ").map_or(rest, |(_, message)| message),
    };
    let mut out = String::new();
    let mut in_number = false;
    for c in message.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('N');
            }
            in_number = true;
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// The normalised warnings in the text qpdf printed when it checked `file`.
pub fn qpdf_complaints(text: &str, file: &Path) -> Vec<String> {
    text.lines().filter(|l| l.starts_with("WARNING")).map(|l| normalise_complaint(l, file)).collect()
}

/// What to make of the output of an operation, judged by qpdf.
pub enum Judgement {
    /// qpdf has nothing to say.
    Clean,
    /// qpdf complains, but only about what it complains about in the inputs
    /// too, so the writer did not add to it.
    InputQuirk,
    /// qpdf complains about something that is not in the inputs.
    Fail(String),
}

/// The one complaint that is a documented limit and not a defect: `merge` does
/// not merge the form fields of the later files (the user is warned), so their
/// widgets are on the pages but not reachable from the output's `/AcroForm`.
const ORPHAN_WIDGETS: &str = "widget annotation is not reachable from /acroform";

/// Judge `output`, made from `inputs`: clean; or not clean but no worse than
/// the inputs (every normalised warning is one of an input's); or a failure.
/// An output qpdf calls broken (exit 2) is a failure unless an input is just
/// as broken. `later_inputs_have_forms` allows the orphan-widget warning for
/// merges whose later inputs have form fields.
pub fn judge_qpdf_output(qpdf: &Path, output: &Path, inputs: &[&Path], later_inputs_have_forms: bool) -> Judgement {
    let verdict = qpdf_check(qpdf, output);
    if verdict.code == 0 {
        return Judgement::Clean;
    }
    let seen: Vec<QpdfVerdict> = inputs.iter().map(|p| qpdf_check_input(qpdf, p)).collect();
    match verdict.code {
        2 if seen.iter().any(|v| v.code == 2) => return Judgement::InputQuirk,
        3 => {}
        _ => return Judgement::Fail(format!("exit {} (no input is that broken)\n{}", verdict.code, verdict.text)),
    }
    let known: std::collections::HashSet<String> =
        seen.iter().zip(inputs).flat_map(|(v, path)| qpdf_complaints(&v.text, path)).collect();
    let new: Vec<String> = qpdf_complaints(&verdict.text, output)
        .into_iter()
        .filter(|c| !known.contains(c) && !(later_inputs_have_forms && c.contains(ORPHAN_WIDGETS)))
        .collect();
    if new.is_empty() {
        Judgement::InputQuirk
    } else {
        Judgement::Fail(format!("warnings the inputs do not give: {new:?}\n{}", verdict.text))
    }
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
